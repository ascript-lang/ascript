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

## `ascript lsp`

Run the language server over stdio (the LSP protocol). Point your editor's generic LSP client at
`ascript lsp` to get diagnostics, document symbols, completion, hover, and go-to-definition.

```text
ascript lsp
```

> [!NOTE] The language server is **static-analysis only** — it lexes and parses your source to
> produce diagnostics and navigation; it never runs the interpreter. Per-document analysis ships
> today; cross-file go-to-definition, rename, and incremental sync are planned enhancements.
