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
if (err == nil) {
  print(`${report.tempC}°C in Lisbon`)
} else {
  print("could not load weather")
}
```

## Why AScript

The guiding model is **"Lua-simple language, Go/Deno-class standard library."** The core stays tiny —
a tree-walking interpreter, ~8 value kinds, gradual contracts, no hidden control flow. The library
does the heavy lifting, because Rust's crate ecosystem makes high-quality batteries cheap.

Design priorities, in strict order: **simplicity → safety → familiarity → performance**.

- **Familiar syntax** — braces, `fn`, arrows, template strings, `for…of`. If you read JavaScript, you read AScript.
- **Gradual type contracts** — optional annotations, checked at runtime as contracts, never erased.
- **Errors as values** — no exceptions; fallible calls return `[value, err]` and the `?` operator propagates. Bugs panic, loudly.
- **Single-threaded async** — `await` any I/O; synchronous code pays nothing; no data races.
- **Batteries included** — JSON, regex, SQLite, crypto, compression, a modern HTTP client, WebSockets, and a TUI.
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
ascript test file.as       # run test(name, fn) cases
ascript lsp                # language server over stdio
```

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
| Core & collections | `std/string` · `std/array` · `std/object` · `std/map` · `std/math` · `std/convert` · `std/bytes` |
| Data & serialization | `std/json` · `std/csv` · `std/toml` · `std/yaml` · `std/encoding` · `std/regex` · `std/uuid` |
| System & files | `std/fs` · `std/env` · `std/process` · `std/crypto` · `std/compress` · `std/sqlite` |
| Time & locale | `std/time` · `std/date` · `std/intl` |
| Networking | `std/net/tcp` · `std/net/http` · `std/http/server` · `std/net/ws` |
| Terminal UI | `std/tui` |

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
ascript run examples/advanced/data_pipeline.as     # CSV → JSON/YAML pipeline
ascript run examples/advanced/sqlite_crud.as       # SQLite with prepared statements & a transaction
ascript run examples/advanced/crypto_and_compress.as

# A JSON API + client (two terminals — see examples/advanced/):
ascript run examples/advanced/http_server.as       # terminal 1
ascript run examples/advanced/http_client.as       # terminal 2
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
