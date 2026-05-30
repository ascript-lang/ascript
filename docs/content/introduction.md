:::eyebrow Introduction

# AScript

AScript is a small, dynamically-typed scripting language with **JavaScript-flavored syntax**,
**optional runtime-checked type contracts**, and a **batteries-included standard library** — all
executed by a tree-walking interpreter compiled into a single Rust binary named `ascript`.

The guiding model is **"Lua-simple language, Go/Deno-class standard library."** The *language core*
stays as simple as Lua — a tree-walker, about eight value kinds, gradual contracts, and no hidden
control flow. The *standard library and tooling* are deliberately rich, because Rust's crate
ecosystem makes high-quality batteries cheap to include.

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

1. **Simplicity** — a beginner can hold the whole language in their head.
2. **Safety** — errors are explicit; mistakes fail loudly, not silently.
3. **Familiarity** — anyone who knows JavaScript can read AScript immediately.
4. **Performance** — adequate for scripting; never at the expense of the above.

## What's in the box

| Capability | Modules |
|---|---|
| Core & collections | `std/string` · `std/array` · `std/object` · `std/map` · `std/math` · `std/convert` · `std/bytes` |
| Data & serialization | `std/json` · `std/csv` · `std/toml` · `std/yaml` · `std/encoding` · `std/regex` · `std/uuid` |
| System & files | `std/fs` · `std/env` · `std/process` · `std/crypto` · `std/compress` · `std/sqlite` |
| Time & locale | `std/time` · `std/date` · `std/intl` |
| Networking | `std/net/tcp` · `std/net/http` · `std/http/server` · `std/net/ws` |
| Terminal UI | `std/tui` |

And the tooling — a runner, a REPL, a formatter, a test runner, and a language server — all live in
the same binary. See [The ascript CLI](cli).

## What AScript is not (v1)

These are deliberate non-goals, not missing features:

- **No static type checking.** Types are runtime contracts, checked at boundaries, never erased.
- **No bytecode VM or JIT.** A tree-walker, by design.
- **No user-level multithreading.** A single-threaded event loop (see [Modules & async](language/modules-async)); no data races, ever.
- **No exceptions.** Recoverable errors are values; bugs panic. See [Errors & results](language/errors).
- **No macros, operator overloading, or metaprogramming.**
- **No tagged-union enums.** Enums are simple named variants; use a class for per-variant data.

> [!NOTE] New here? Start with [Getting started](getting-started) for installation and your first
> program, then skim [Syntax & control flow](language/syntax). If you already know JavaScript, the
> [Standard library overview](stdlib/overview) is the fastest path to being productive.
