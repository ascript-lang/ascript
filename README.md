<div align="center">

# AScript

**A multi-paradigm scripting language with gradual types, structured concurrency, and a Go-class standard library.**

[![CI](https://github.com/ascript-lang/ascript/actions/workflows/ci.yml/badge.svg)](https://github.com/ascript-lang/ascript/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-ascript--lang.github.io-3b82f6)](https://ascript-lang.github.io/ascript/)
[![Vibe-coded](https://img.shields.io/badge/vibe--coded-%E2%9C%A8-ff69b4)](https://github.com/ascript-lang/ascript/commits/main)

AScript is a gradually-typed, multi-paradigm scripting language with JavaScript-flavored syntax,
runtime-checked type contracts (plus an advisory static checker), first-class structured concurrency,
and a batteries-included standard library — all in a single Rust binary.

_Vibe-coded: designed and built end-to-end with AI (Claude), human-directed._

</div>

```ascript
import { get } from "std/net/http"

// Typed fields + a default, validated at the boundary.
class User {
  id: number
  name: string
  role: string = "guest"
}

async fn fetchUser(id: number): Result<User> {
  let resp = await get(`https://api.example.com/users/${id}`)?  // ? propagates errors
  return await resp.json(User)                                  // parse + validate in one step
}

let user = await fetchUser(42)!   // ! unwraps, or panics (recoverably)
print(`${user.name} — ${user.role}`)
```

## Why AScript

The guiding model is **a focused core with a Go-class standard library.** The core stays
approachable — a small set of value kinds, gradual type contracts, no hidden control flow — but it is
genuinely multi-paradigm: object-oriented (classes, inheritance, `instanceof`), functional (closures,
pattern matching, generators, destructuring, ranges, lazy streams), and concurrent (`async`/`await`,
structured concurrency, channels, durable workflows). It runs on a register-light **bytecode VM** with
inline caches and a cycle-collecting GC. The library does the heavy lifting, because Rust's crate
ecosystem makes high-quality batteries cheap.

Design priorities, in strict order: **simplicity → safety → familiarity → performance**. "Simplicity"
here means a core you can hold in your head and no hidden control flow — not a feature-poor language.

- **Familiar syntax** — braces, `fn`, arrows, template strings, `for…of`. If you read JavaScript, you read AScript.
- **Gradual type contracts** — optional annotations, checked at runtime as contracts, never erased. Includes the nullable suffix `T?` (≡ `T | nil`) and typed class fields (required, optional, defaulted) checked on assignment.
- **Errors as values** — no exceptions; fallible calls return `[value, err]`; the `?` operator propagates and the `!` force-unwrap asserts success (panicking, recoverably, with the original message). Bugs panic, loudly.
- **Shape validation** — turn untrusted data into checked instances: `ClassName.from(obj)` validates a raw object (recursing into nested classes, `array<Class>`, and `map<K, Class>`), and the typed parse `json.parse(text, Class)` / `resp.json(Class)` fuses decode + validation into one result — `let user = await resp.json(User)?`.
- **Composable schema validation** — `std/schema` lets you build and compose schemas independently of any class: `schema.object({name: schema.minLength(schema.string(), 1), age: schema.min(schema.number(), 0)})`, with `min`/`max`, `minLength`/`maxLength`, `pattern`, `refine` (custom async predicates), `default`, `optional`, `union`, `oneOf`, and coercion (`{coerce: true}`). Refiners and `parse` also chain as **fluent methods** — `schema.string().minLength(3).maxLength(12).pattern("^[a-z0-9_]+$").parse(input)` — equivalent to and interoperable with the free-function form. `schema.fromClass(Class)` derives a schema from class field declarations. Pass a schema to `json.parse(text, schema)` to fuse JSON decoding and validation into one Tier-1 pair.
- **Pattern matching** — `match` is an expression with structural patterns: wildcard `_`, ranges (`1..=9`, `0..10`), array destructuring (`[a, b, ...rest]`), object destructuring (`{key, role: "admin", ...rest}`), `|` alternatives, and `if` guards. Bare identifiers use **Option C**: a name already defined in scope is compared (`==`); an undefined name binds the subject. The `[value, err]` idiom (`[v, nil] => …`) and enum variants work naturally. See [the docs](docs/content/language/classes-enums.md).
- **Ranges** — `a..b` (exclusive) and `a..=b` (inclusive) are sequences whose direction follows the bounds (`10..1` counts down). A signed `step` (`1..10 step 2`, `10..1 step -2`) sets the stride; omit it and the direction is inferred. A range as a value materializes to `array<number>`; `for`-range stays lazy; stepped ranges also work in `match` patterns (strided membership). See [the docs](docs/content/language/syntax.md#ranges).
- **Destructuring** — pull fields out of an object or instance by key with `let {a, b as local, "k" as v} = obj`; missing keys bind `nil`.
- **Default parameters** — `fn f(a, b = 10)` (also arrows, methods, `init`, `async fn`, `fn*`): evaluated at call time, left-to-right, can reference earlier params, typed defaults are contract-checked; an explicit `nil` suppresses the default. A required param may not follow a defaulted one.
- **`instanceof`** — `x instanceof C` tests class membership up the superclass chain (a comparison-tier operator); a non-instance left side is `false`, never a panic.
- **Map literals** — `#{ keyExpr: value, … }` builds a `map` directly (no `std/map` import); the key is an expression, keys may be any hashable value, `#{}` is empty, repeated keys are later-wins.
- **Records** — a class that declares fields but writes no `init` auto-derives a **positional constructor** over its fields in declaration order (base-class fields first); a defaulted field becomes an optional trailing parameter, each arg is contract-checked. Field defaults may be any expression, including ranges (`xs: array<number> = 1..=3`).
- **`object.freeze` / `object.isFrozen`** — shallow, one-way runtime freeze of a container or instance (returns it for chaining); any later in-place mutation panics. `deepClone` of a frozen value is unfrozen.
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
ascript run program.as     # run a program (compiles to bytecode, runs on the VM)
ascript build program.as   # compile to bytecode → program.aso
ascript run program.aso    # run compiled bytecode (no compile step)
ascript repl               # interactive REPL
ascript fmt file.as        # format in place
ascript check file.as      # static check (syntax + lints)
ascript check --fix *.as   # apply safe autofixes (unused-import removal)
ascript test file.as       # run test(name, fn) cases
ascript lsp                # language server over stdio (cross-file nav + rename)

ascript add ../util        # add a dependency (git/url/path) → ascript.toml + lock
ascript install            # resolve + fetch deps + write ascript.lock
ascript install --locked   # CI: install EXACTLY from the lock (no network)
ascript update [name]      # re-resolve + rewrite the lock
ascript tree               # print the resolved dependency graph
ascript verify             # re-hash the cache against the lock integrity
```

### Packages & dependencies

Declare third-party code in `ascript.toml` (`[package]` + `[dependencies]`) and
resolve it reproducibly from **git / URL / local path** via Go-style Minimal
Version Selection — no central registry to operate. Dependencies are cached
content-addressed (`$ASCRIPT_CACHE` / XDG), locked with fail-closed `asum1-`
integrity in `ascript.lock`, and loaded through a **bare specifier**
(`import "http"`) on both engines. There are **no install scripts** — packages
are pure `.as` source, the hashed contract. `ascript run`/`test` implicitly
ensure the lock (`--locked` for hermetic CI). See the
[packages guide](docs/content/packages.md).

### Runtime & performance

`ascript run` compiles your program to bytecode and executes it on the **bytecode VM** — the default
and only production engine. Call frames are heap-allocated, so deep recursion is bounded by heap, not
the native stack. Adaptive specialization (inline caches + adaptive arithmetic) makes it roughly
**2–3× faster** than the legacy tree-walker on compute-bound code (geomean ~2.5× in the repo's
`std/bench` suite). Memory is managed by a **cycle-collecting GC**: acyclic data is freed immediately
and deterministically, reference cycles are reclaimed by periodic collection, and native OS resources
(files, sockets, child processes) are dropped immediately — never on the GC's schedule.

```bash
ascript build app.as            # → app.aso
ascript build app.as -o out.aso # choose the output path
ascript run app.aso             # run the compiled artifact
```

`.aso` files are a versioned, verified compilation cache / distributable artifact (not a stable
cross-version format — a version bump rejects old files with a "recompile from source" error). The
legacy tree-walker is retained as a differential oracle and a debugging escape hatch — run it with
`ascript run file.as --tree-walker` (or `ASCRIPT_ENGINE=tree-walker`). See
[Compilation & runtime](docs/content/runtime.md) for the full picture.

### Checking

`ascript check` statically checks `.as` files (syntax errors + lints) and reports
**all** diagnostics with an exit code suited to CI: `0` clean, `1` on a lint failure,
`2` on a usage error (e.g. an unknown rule). It includes an **advisory gradual type
checker** (`type-mismatch` / `type-error` / `possibly-nil`, all default-Warning)
that predicts likely runtime contract violations — annotation mismatches, provably
ill-typed operations, and unguarded `T?` dereferences — while staying silent on
idiomatic untyped code (only *provably* wrong code is flagged).

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
[checker design spec](superpowers/specs/2026-06-02-checker-design.md) for the
full rule-code list and details.

`ascript check --fix` (or `--fix-dry-run` to preview) applies the safe, idempotent
autofixes — currently **unused-import** removal — and re-evaluates the exit status
against the post-fix analysis. The `call-arity` lint reaches across modules and to
**constructor**, **method**, and **imported `std/*`** calls (zero false positives); the
language server's cross-file index extends this and powers go-to-definition,
find-references, workspace symbols, and **rename across files**, alongside the full
modern LSP surface (hover types, signature help, semantic tokens, inlay hints, code
lenses, call/type hierarchy, and more). It stays responsive under rapid editing and
degrades gracefully on very large files.

See [editor setup](docs/content/tooling/editor-setup.md) for VS Code, Zed, and Neovim,
and the [LSP capability reference](docs/content/tooling/lsp-capabilities.md) for every
method the server answers.

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
| Data & serialization | `std/json` · `std/csv` · `std/toml` · `std/yaml` (all with typed `parse(text, Class\|schema)`) · `std/msgpack` · `std/cbor` (binary) · `std/encoding` · `std/regex` · `std/uuid` · `std/url` · `std/decimal` (exact 96-bit decimal arithmetic) |
| Validation & schema | `std/schema` (composable validators: object/array/map/union/oneOf/optional, constraints, refine, coerce, fromClass, `parseAll` collect-all-errors) |
| System & files | `std/fs` · `std/env` · `std/io` · `std/process` · `std/crypto` · `std/compress` (gzip/deflate/zip · zstd · brotli · tar) · `std/sqlite` |
| Databases | `std/postgres` · `std/redis` (async network clients; native-resource handles) |
| Utilities | `std/lru` (bounded LRU cache) · `std/events` (event-emitter) · `std/template` (`{{name}}` templating) |
| Host & OS | `std/os` (pid · platform · arch · cpuCount · hostname · tempDir; live metrics via `sysinfo` feature: memory · swap · cpuUsage · loadAvg · disks · uptime · networkInterfaces · localIp) |
| CLI & terminal | `std/cli` (declarative arg parser) · `std/color` (ANSI colors & styles, NO_COLOR-aware) |
| Time & locale | `std/time` (wall clock, sleep, `interval` · `debounce` · `throttle`) · `std/date` · `std/intl` |
| Networking | `std/net` (DNS: `lookup` · `lookupOne`) · `std/net/tcp` · `std/net/udp` (datagram sockets) · `std/net/http` · `std/http/server` (verb methods: `get`/`post`/`put`/`patch`/`delete`/`head`/`options`; schema-validated typed routes) · `std/net/ws` |
| Concurrency | `std/task` (`spawn` · `gather` · `race` · `timeout` · `retry` over `future<T>`) · `std/sync` (FIFO channels · counting semaphore · token-bucket rate limiter) |
| Logging | `std/log` (`debug` · `info` · `warn` · `error`; human/json, structured fields) |
| Observability | `std/telemetry` (tracing spans · metrics: counter/histogram/gauge · analytics: `capture`/`identify`; hand-rolled OTLP HTTP/JSON · Sentry · PostHog exporters; opt-in `telemetry` feature, no-op until `init`) |
| AI / LLM | `std/ai` (unified multi-provider client wrapping `genai`: OpenAI · OpenAI-compat swarm · Anthropic · Gemini · Bedrock SigV4 · Vertex ADC · Azure; `"provider:model"` + env creds · `generate` (Tier-1) · `stream` (generators/`for await`) · class/`std/schema` structured output + JSON-Schema projector · in-interpreter tool loop · `embed`/`embedMany`; OTel GenAI spans via the telemetry hook; opt-in `ai` feature) |
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
ascript run examples/ranges.as                     # ranges: ..=, signed step, sequence direction
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
cargo test                         # full suite (~2,565 tests, all features)
cargo test --no-default-features   # core language only (~1,854 tests)
cargo clippy --all-targets         # lint — kept clean in both feature configs
```

Architecture and contributor guidance live in [CLAUDE.md](CLAUDE.md); the full design spec and
milestone history are under [`superpowers/`](superpowers/).

## License

See the repository for license details.
