::: eyebrow Standard library

# CLI & terminal

Two lightweight modules for writing command-line tools: `std/cli` for declarative argument parsing and `std/color` for ANSI terminal styling. Both are part of the core build (no Cargo feature gate).

## std/cli

Declarative command-line argument parser. `cli.parse` reads a spec object you define and returns a structured result — flags, options, positionals, and an optional subcommand. It has no external crate dependency.

> [!TIER1] `cli.parse` returns a `[result, err]` pair. Parse errors (unknown flags, missing required positionals, and so on) surface as `[nil, err]` rather than panicking.

### cli.parse

Parses command-line arguments against a declarative spec.

- **spec** `object` — describes the CLI (see the spec shape below).
- **args** `array<string>` (optional) — the arguments to parse. When omitted or `nil`, defaults to `env.args()` (the script's trailing CLI arguments).
- **Returns** `[result, err]`.

#### Spec shape

```ascript
{
  name: "mytool",           // program name shown in usage (optional)
  flags:   [{ name: "verbose", short: "v", help: "..." }],
  options: [{ name: "output",  short: "o", default: "out.txt", help: "..." }],
  positionals: [{ name: "input", required: true, help: "..." }],
  subcommands: [
    { name: "build", flags: [...], options: [...], positionals: [...] }
  ]
}
```

| Field | Type | Notes |
| --- | --- | --- |
| `name` | `string` | Shown in the generated usage line. |
| `flags` | `array` | Boolean switches. Each entry: `name` (required), `short` (optional, single char), `help` (optional). |
| `options` | `array` | Named values. Each entry: `name`, `short`, `default` (optional), `help`. |
| `positionals` | `array` | Ordered positional arguments. Each entry: `name`, `required` (bool), `help`. |
| `subcommands` | `array` | Subcommand objects with their own `name`, `flags`, `options`, `positionals`. |

#### Result shape (on success)

```ascript
{
  flags:       { verbose: false, ... },
  options:     { output: "out.txt", ... },
  positionals: { input: "file.as", ... },
  subcommand:  nil | { name: "build", flags: {...}, options: {...}, positionals: {...} },
  help:        nil | "<usage text>",
}
```

- All flags start as `false`; a present flag flips to `true`.
- All options start at their declared default, or `nil` if no default was given.
- `subcommand` is `nil` when no subcommand was matched.
- `help` is a non-empty string when `--help` or `-h` was passed; the caller should print it and exit. `err` is `nil` in this case.

#### Special behaviors

- **`--help` / `-h`** — triggers help mode: returns a generated usage string in `result.help` with `err == nil`. The caller is responsible for printing it.
- **`--`** (double-dash) — everything after this token is treated as a positional, even if it starts with `-` or `--`. Subcommand matching and help detection stop at `--`.
- **`--name=value`** — inline value syntax is supported for options.

```ascript
import * as cli from "std/cli"

let spec = {
  name: "greet",
  flags:   [{ name: "verbose", short: "v", help: "enable verbose output" }],
  options: [{ name: "format",  short: "f", default: "text", help: "output format" }],
  positionals: [{ name: "name", required: true, help: "the name to greet" }],
}

let [result, err] = cli.parse(spec)
if (err != nil) {
  print("error: " + err.message)
  exit(1)
}

if (result.help != nil) {
  print(result.help)
  exit(0)
}

let name = result.positionals.name
let format = result.options.format
let verbose = result.flags.verbose

if (verbose) { print("format: " + format) }
print("Hello, " + name + "!")
```

Running this as `ascript run greet.as -- Alice -v -f json` would set `verbose = true`, `format = "json"`, and `name = "Alice"`. Running with `--help` would print the generated usage text.

#### Subcommands example

```ascript
import * as cli from "std/cli"

let spec = {
  name: "tool",
  subcommands: [
    {
      name: "build",
      flags: [{ name: "release", short: "r", help: "build in release mode" }],
      positionals: [{ name: "target", required: false, help: "target name" }],
    },
  ],
}

let [result, err] = cli.parse(spec)
if (err != nil) { print(err.message); exit(1) }

if (result.subcommand != nil) {
  let sub = result.subcommand
  print("subcommand: " + sub.name)
  print("release: " + sub.flags.release)
}
```

## std/color

Dependency-free ANSI SGR terminal styling. Emits raw escape sequences for foreground colors, text styles, and 24-bit truecolor. Respects the [NO_COLOR](https://no-color.org) standard: when the `NO_COLOR` environment variable is set to a non-empty value, all styling helpers return the original string unchanged. `color.strip` always strips regardless of `NO_COLOR`.

> [!NOTE] `std/color` is part of the core build (no Cargo feature gate required).

### Foreground colors

All foreground helpers take a single string argument and return the styled string.

`color.black` · `color.red` · `color.green` · `color.yellow` · `color.blue` · `color.magenta` · `color.cyan` · `color.white` · `color.gray` (alias: `color.grey`)

```ascript
import * as color from "std/color"
print(color.red("error: something went wrong"))
print(color.green("success"))
print(color.yellow("warning: check this"))
print(color.gray("(debug info)"))
```

### Text styles

All style helpers take a single string argument and return the styled string.

`color.bold` · `color.dim` · `color.italic` · `color.underline`

```ascript
import * as color from "std/color"
print(color.bold("important"))
print(color.underline("link text"))
print(color.dim("less important"))
```

### Composing styles

Wrap one helper with another to combine effects:

```ascript
import * as color from "std/color"
print(color.bold(color.red("critical error")))
```

### color.rgb

Applies a 24-bit truecolor foreground using RGB values.

- **r** `number` — red component, integer `0..=255`.
- **g** `number` — green component, integer `0..=255`.
- **b** `number` — blue component, integer `0..=255`.
- **s** `string` — the text to style.
- **Returns** `string`.

> [!TIER2] Out-of-range or fractional RGB values are a Tier-2 panic.

```ascript
import * as color from "std/color"
print(color.rgb(255, 165, 0, "orange text"))
```

### color.bgRgb

Applies a 24-bit truecolor **background** using RGB values. Parameters are the same as `color.rgb`.

```ascript
import * as color from "std/color"
print(color.bgRgb(0, 0, 128, "dark blue background"))
```

### color.strip

Removes all ANSI CSI SGR sequences from a string. Always strips regardless of `NO_COLOR`.

- **s** `string` — the string to strip.
- **Returns** `string` — the plain text without any escape codes.

```ascript
import * as color from "std/color"
let styled = color.bold(color.red("hello"))
let plain  = color.strip(styled)
// plain == "hello"
assert(plain == "hello")
```

Use `color.strip` when you need the raw character count of a styled string or when writing to a file or pipe that should not receive escape codes.
