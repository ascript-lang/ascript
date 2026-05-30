:::eyebrow Introduction

# The ascript CLI

Everything ships in a single binary. There is no separate toolchain to assemble — the runner, REPL,
formatter, test runner, and language server are all subcommands of `ascript`.

## `ascript run`

Execute a `.as` program. Imports resolve relative to the entry file.

```text
ascript run path/to/program.as
```

The program's `print` output goes to stdout. A [panic](language/errors) (an unrecoverable error)
prints a diagnostic with a source span and exits with a non-zero status.

## `ascript repl`

Start the interactive read-eval-print loop. Expression results are printed automatically; `let` and
`const` bindings persist for the session.

```text
ascript repl
```

```text
> let xs = [1, 2, 3]
> import * as array from "std/array"
> array.reduce(xs, (a, b) => a + b, 0)
6
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

## `ascript lsp`

Run the language server over stdio (the LSP protocol). Point your editor's generic LSP client at
`ascript lsp` to get diagnostics, document symbols, completion, hover, and go-to-definition.

```text
ascript lsp
```

> [!NOTE] The language server is **static-analysis only** — it lexes and parses your source to
> produce diagnostics and navigation; it never runs the interpreter. Per-document analysis ships
> today; cross-file go-to-definition, rename, and incremental sync are planned enhancements.
