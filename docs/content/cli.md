:::eyebrow Introduction

# The ascript CLI

Everything ships in a single binary. There is no separate toolchain to assemble — the runner, REPL,
formatter, test runner, and language server are all subcommands of `ascript`.

## `ascript run`

Execute a `.as` program. Imports resolve relative to the entry file.

```text
ascript run path/to/program.as
```

`run` compiles the program to bytecode and executes it on the [bytecode VM](runtime) — the default
engine. The program's `print` output goes to stdout. A [panic](language/errors) (an unrecoverable
error) prints a diagnostic with a source span and exits with a non-zero status.

Pass `--tree-walker` (before the file) to run on the legacy tree-walker engine instead — a debugging
and differential aid; see [Compilation & runtime](runtime).

## `ascript build`

Compile a `.as` program to a `.aso` bytecode file, then run the artifact with no compile step.

```text
ascript build program.as              # → program.aso
ascript build program.as -o out.aso   # choose the output path
ascript run program.aso               # run the compiled bytecode
```

`.aso` is a versioned, verified compilation cache — see [Compilation & runtime](runtime) for what it
is, when to use it, and why it is not a stable cross-version format.

## `ascript repl`

Start the interactive read-eval-print loop. Expression results are printed automatically. Session
state persists across lines — `let`/`const` bindings, function definitions, and imports all stay
available for the rest of the session.

Multi-line input continues automatically: when a line leaves a delimiter unclosed (or a string /
template unterminated), the REPL switches to a `..` continuation prompt and keeps reading until the
input is complete, then runs the whole buffer at once. Press `Ctrl-C` to cancel a partial entry
(this clears the buffer rather than exiting).

```text
ascript repl
```

The REPL runs on the [bytecode VM](runtime); each entry is compiled and executed against the
persistent session globals. Pass `--tree-walker` to use the legacy engine instead.

```text
>> let xs = [1, 2, 3]
>> import * as array from "std/array"
>> array.reduce(xs, (a, b) => a + b, 0)
6
>> class Point {
..   x: number
..   y: number
.. }
>> let p = Point.from({x: 3, y: 4})
>> p.x + p.y
7
```

## `ascript fmt`

Format one or more source files **in place** to the canonical style.

```text
ascript fmt src/main.as src/util.as
```

The formatter is built on the same parser as the runtime, so formatting never changes a program's
meaning — only its layout.

## `ascript test`

Run the test cases registered by `test(name, fn)` across one or more files. Each test runs under an
internal [`recover`](language/errors) boundary, so a failing assertion reports as a failure rather
than aborting the run.

```ascript
// math_test.as
import * as math from "std/math"

test("abs of a negative", () => {
  assert(math.abs(-5) == 5, "abs should be 5")
})

test("max picks the largest", () => {
  assert(math.max(1, 9, 4) == 9)
})
```

```text
ascript test math_test.as
```

```text
ok. 2 passed; 0 failed
```

A non-zero exit status indicates at least one failure, which makes `ascript test` suitable for CI.

### Rich assertions with std/assert

For test bodies that need deep equality, container membership, approximate equality, or panic
capture, import [`std/assert`](stdlib/assert):

```ascript
import * as assert from "std/assert"

test("deep array equality", () => {
  assert.eq([1, [2, 3]], [1, [2, 3]])         // deep structural equality
  assert.contains("hello world", "world")      // substring check
  assert.approxEq(0.1 + 0.2, 0.3)             // float tolerance (1e-9)
})

test("expected error is thrown", () => {
  let e = assert.throws(() => assert.eq(1, 99))
  assert.contains(e.message, "assert.eq failed")
})
```

`std/assert` is distinct from the global `assert(cond, msg?)` builtin — both work in test bodies,
and they can coexist (import under a different alias if needed: `import * as A from "std/assert"`).

## `ascript check`

Statically check `.as` files — syntax errors plus a set of lints (unused bindings, shadowing,
unawaited futures, ignored results, and more) — without running them. It shares its analysis core
with the language server, so the diagnostics you see here match those in your editor.

```text
ascript check src/main.as src/util.as
ascript check src/*.as --deny unused-binding --allow shadowing --deny-warnings
```

Lint levels can be tuned per invocation (`--deny`/`--warn`/`--allow`) or via an `ascript.toml`
discovered by walking up from the checked file. A non-zero exit status indicates problems were
found, which makes `ascript check` suitable for CI.

Three rules cover ranges and import/propagation hygiene (all default to **Warning**, all configurable
via `--deny`/`--warn`/`--allow` or the `[lint]` table):

- **`range-step`** — a statically-detectable bad range: a `step` of `0` (or a non-finite literal), or a
  step whose sign disagrees with the bounds so the range can never progress. It also flags a *float*
  `step` inside a `match` pattern as unreliable (the stride test there is exact float equality).
- **`invalid-propagate`** — a postfix `?` (Result propagation) used where it cannot apply, e.g. outside
  a function or on an expression that is never a `[value, err]` pair.
- **`unresolved-import`** — an `import … from "std/…"` naming a std module that does not exist (e.g. a
  typo like `"std/maths"`). **V1 limitation:** only `std/*` specifiers are checked; relative file paths
  (`"./mod"`, `"mod.as"`) are not yet resolved (the analysis is path-less), so they are left untouched.

## `ascript lsp`

Run the language server over stdio (the LSP protocol). Point your editor's generic LSP client at
`ascript lsp` to get diagnostics, document symbols, completion, hover, and go-to-definition.

```text
ascript lsp
```

> [!NOTE] The language server is **static-analysis only** — it lexes and parses your source to
> produce diagnostics and navigation; it never runs the interpreter. Per-document analysis ships
> today; cross-file go-to-definition, rename, and incremental sync are planned enhancements.
