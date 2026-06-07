:::eyebrow Introduction

# AScript

AScript is a gradually-typed, multi-paradigm scripting language with **JavaScript-flavored syntax**,
**runtime-checked type contracts** (plus an advisory static checker), **first-class structured
concurrency**, and a **batteries-included standard library** — all compiled to bytecode and executed
by a virtual machine inside a single Rust binary named `ascript`.

The guiding model is **a focused core with a Go-class standard library.** The *language core* stays
approachable — a small set of value kinds, gradual type contracts, and no hidden control flow — but it
is genuinely multi-paradigm: object-oriented (classes, inheritance, `instanceof`), functional
(closures, pattern matching, generators, destructuring, ranges, lazy streams), and concurrent
(`async`/`await`, structured concurrency, channels, durable workflows). It runs on a [bytecode
VM](runtime) with inline caches and a cycle-collecting GC. The *standard library and tooling* are
deliberately rich, because Rust's crate ecosystem makes high-quality batteries cheap to include.

```ascript
import * as json from "std/json"
import { get } from "std/net/http"

async fn weather(city: string): Result<object> {
  let [resp, err] = await get(`https://api.example.com/weather?q=${city}`)
  if (err != nil) { return Err(err.message) }
  return await resp.json()
}

let [report, err] = await weather("Lisbon")
print(err == nil ? `${report.tempC}°C in Lisbon` : "could not load weather")
```

## Design priorities

In strict order — when two goals conflict, the earlier one wins:

1. **Simplicity** — the core stays small and predictable; no hidden control flow.
2. **Safety** — errors are explicit; mistakes fail loudly, not silently.
3. **Familiarity** — anyone who knows JavaScript can read AScript immediately.
4. **Performance** — adequate for scripting; never at the expense of the above.

## What's in the box

| Capability | Modules |
|---|---|
| Core & collections | `std/string` · `std/array` · `std/object` · `std/map` · `std/set` · `std/math` · `std/decimal` · `std/convert` · `std/bytes` · `std/lru` |
| Validation & schemas | `std/schema` (composable schemas, refiners, coercion, `fromClass`) |
| Data & serialization | `std/json` · `std/csv` · `std/toml` · `std/yaml` · `std/msgpack` · `std/cbor` · `std/encoding` · `std/regex` · `std/uuid` · `std/url` |
| Concurrency & streams | `std/task` · `std/sync` · `std/stream` · `std/events` |
| System & files | `std/fs` · `std/io` · `std/env` · `std/os` · `std/process` · `std/crypto` · `std/compress` |
| Databases | `std/sqlite` · `std/postgres` · `std/redis` |
| Time & locale | `std/time` · `std/date` · `std/intl` |
| Networking | `std/net` · `std/net/tcp` · `std/net/udp` · `std/net/http` · `std/http/server` · `std/net/ws` |
| CLI & terminal | `std/cli` · `std/color` · `std/tui` · `std/template` |
| AI & observability | `std/ai` · `std/telemetry` · `std/log` |
| Durable execution | `std/workflow` (event-sourced, replayable workflows) |
| Testing & benchmarks | `std/assert` · `std/bench` |

And the tooling — a runner, a REPL, a formatter, a test runner, and a language server — all live in
the same binary. See [The ascript CLI](cli).

## What AScript is not

These are deliberate non-goals, not missing features:

- **No mandatory, blocking type checker.** Types are enforced as *runtime contracts*, checked at boundaries and never erased. A static checker (`ascript check`) does flag likely type errors — `type-mismatch`, `possibly-nil`, and friends — but only as **advisory** diagnostics that never block a run. See [Type contracts](language/type-contracts).
- **No shared-memory multithreading.** A single-threaded cooperative event loop per isolate (see [Modules & async](language/modules-async)) with structured concurrency via `std/task`; no data races, ever. Multi-core parallelism comes from shared-nothing [workers](language/workers) — separate isolates that share no memory.
- **No exceptions.** Recoverable errors are values; bugs panic. See [Errors & results](language/errors).
- **No macros, operator overloading, or metaprogramming.**
- **No tagged-union enums.** Enums are simple named variants; use a class for per-variant data.

> The default engine is a **bytecode VM** with inline caches and a cycle-collecting GC. The original
> tree-walking interpreter is retained as a byte-for-byte differential oracle and a `--tree-walker`
> debug engine — see [The runtime](runtime).

> [!NOTE] New here? Start with [Getting started](getting-started) for installation and your first
> program, then skim [Syntax & control flow](language/syntax). If you already know JavaScript, the
> [Standard library overview](stdlib/overview) is the fastest path to being productive.
