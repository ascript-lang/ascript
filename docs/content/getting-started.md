:::eyebrow Introduction

# Getting started

## Build the binary

AScript builds from source with a standard Rust toolchain (stable). From the repository root:

```text
cargo build --release
```

This produces the `ascript` binary at `target/release/ascript`. The default build includes the full
standard library; you can trim it with Cargo features (see below).

> [!NOTE] To build the bare language with no batteries (useful for embedding), use
> `cargo build --no-default-features`. Each standard-library domain is a separate feature flag.

## Your first program

Create `hello.as`:

```ascript
fn greet(name: string): string {
  return `Hello, ${name}!`
}

print(greet("world"))
```

Run it:

```text
ascript run hello.as
```

```text
Hello, world!
```

## A taste of the language

```ascript
// Bindings: let is mutable, const is not.
let total = 0
const tax = 0.2

// Arrays, objects, and for…of.
const cart = [
  { name: "book",  price: 12 },
  { name: "lamp",  price: 30 },
]

for (item of cart) {
  total += item.price
}
print(`subtotal: ${total}`)            // subtotal: 42
print(`with tax: ${total * (1 + tax)}`) // with tax: 50.4

// Errors are values, not exceptions.
import * as convert from "std/convert"
let [n, err] = convert.parseNumber("not a number")
if (err == nil) {
  print(n)
} else {
  print(`parse failed: ${err.message}`)   // parse failed: cannot parse 'not a number' as a number
}
```

## The REPL

For quick experiments, start the interactive read-eval-print loop:

```text
ascript repl
```

Expressions are evaluated and printed; `let`/`const` bindings, functions, and imports persist across
lines within the session. Multi-line constructs (a `class`, a `fn` body, a multi-line object/array)
continue on a `..` prompt until the input is complete — press `Ctrl-C` to cancel a partial entry.
See [The ascript CLI](cli) for the formatter, test runner, and language server.

## Feature flags

The standard library is split into Cargo features, all enabled by `default`:

| Feature | Modules it enables |
|---|---|
| `data` | json, csv, toml, yaml, encoding, regex, uuid, bytes |
| `datetime` | date |
| `intl` | intl |
| `sys` | fs, env, process |
| `crypto` | crypto |
| `compress` | compress |
| `sql` | sqlite |
| `net` | net/tcp, net/http, http/server, net/ws |
| `tui` | tui |
| `lsp` | the `ascript lsp` language server |

Building with a subset (for a smaller binary or fewer dependencies) cleanly omits the rest:

```text
cargo build --no-default-features --features "data,sys,net"
```

> [!NOTE] `std/math`, `std/string`, `std/array`, `std/object`, `std/map`, `std/convert`, and
> `std/time` are always available — they have no feature gate.

## Where to go next

- [Syntax & control flow](language/syntax) — the whole grammar in one page.
- [Errors & results](language/errors) — the `[value, err]` convention and the `?` operator.
- [Standard library overview](stdlib/overview) — how imports and error tiers work.
- [Examples](examples) — complete, runnable programs for every domain.
