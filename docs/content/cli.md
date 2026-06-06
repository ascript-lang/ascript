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

It also runs an **advisory gradual type checker** that emits `type-mismatch`, `type-error`, and
`possibly-nil` (all default-Warning) by predicting a likely runtime [contract](language/type-contracts)
violation ahead of time — annotation mismatches, provably ill-typed operations, and unguarded `T?`
dereferences. It is gradual: idiomatic untyped code stays silent, only *provably* wrong code is
flagged. See [Type contracts → Static type checking](language/type-contracts) for the full surface
and the narrowing rules.

```text
ascript check src/main.as src/util.as
ascript check src/*.as --deny unused-binding --allow shadowing --deny-warnings
```

Lint levels can be tuned per invocation (`--deny`/`--warn`/`--allow`) or via an `ascript.toml`
discovered by walking up from the checked file. A non-zero exit status indicates problems were
found, which makes `ascript check` suitable for CI.

### Autofix (`--fix` / `--fix-dry-run`)

```text
ascript check --fix src/*.as          # apply safe autofixes in place
ascript check --fix-dry-run src/*.as  # preview the changes (unified diff) without writing
```

`--fix` applies the **safe, unambiguous** autofixes — currently the removal of an **unused import**
(removing the whole `import` statement, or a single clause of a multi-name `import { a, b }` list,
keeping the list well-formed). It rewrites the file in place, prints `fixed N issue(s)`, and
re-evaluates the exit status against the *post-fix* analysis (a file whose only issue was a fixed
import exits **0**). Re-running `--fix` is **idempotent**. `--fix-dry-run` prints a unified diff
(or, with `--json`, the planned edits) and never touches the file; the two flags are mutually
exclusive. `unused-binding` removal is deliberately **not** auto-applied (it could drop a
side-effecting initializer like `let x = doWork()`), though the editor still offers it as a
code-action.

Several structural rules cover ranges, import/propagation hygiene, calls, enums, and classes (all
default to **Warning**, all configurable via `--deny`/`--warn`/`--allow` or the `[lint]` table):

- **`range-step`** — a statically-detectable bad range: a `step` of `0` (or a non-finite literal), or a
  step whose sign disagrees with the bounds so the range can never progress. It also flags a *float*
  `step` inside a `match` pattern as unreliable (the stride test there is exact float equality).
- **`invalid-propagate`** — a postfix `?` (Result propagation) used where it cannot apply, e.g. outside
  a function or on an expression that is never a `[value, err]` pair.
- **`unresolved-import`** — an `import … from "std/…"` naming a std module that does not exist (e.g. a
  typo like `"std/maths"`). **V1 limitation:** only `std/*` specifiers are checked; relative file paths
  (`"./mod"`, `"mod.as"`) are not yet resolved (the analysis is path-less), so they are left untouched.
- **`call-arity`** — a call with the wrong number of arguments where the callee is statically
  certain. This covers: a directly-named **function** (default params widen the accepted range, a
  `...rest` makes the max unbounded); a **constructor** `C(args)` against the class's `init` or
  auto-derived field arity; a **method** `recv.m(args)` where the receiver's class is provable
  (only `self` in a method, or a `let`/`const` bound directly to `C(...)` and never reassigned);
  and an **imported `std/*` function** with too few args (a guaranteed runtime panic — native
  functions ignore surplus args, so a too-many call is never flagged). Cross-file calls to a
  *file-module* exported function are checked in the editor (the language server's workspace index
  knows the target's signature). Every case stays **zero-false-positive**: any uncertainty skips
  the call.
- **`unknown-enum-variant`** — accessing a variant that the enum doesn't declare.
- **`duplicate-member`** — two fields/methods with the same name in one class.
- **`super-misuse`** — `super` used in a class that has no superclass.
- **`field-default-type`** — a class field's literal default contradicts its declared type.

## `ascript lsp`

Run the language server over stdio (the LSP protocol). Point your editor's generic LSP client at
`ascript lsp` to get diagnostics, document symbols, completion, hover, go-to-definition,
**find-references**, **workspace symbols**, **rename**, **document and range formatting**, and
**code actions** — with navigation working **across files**.

```text
ascript lsp
```

The server builds a **cross-file workspace index** (warmed from the workspace root on startup,
re-indexed incrementally as you type) so navigation spans modules:

- **go-to-definition** on a use of an imported name jumps to the defining file;
- **find-references** lists a symbol's uses across its file and every file that imports it;
- **workspace symbols** (`workspace/symbol`) searches every `.as` file in the workspace;
- **rename** rewrites a symbol's declaration, the import clauses that name it, and its use sites,
  refusing the edit if a touched file has a parse error or the new name collides with an existing
  binding;
- a transient parse error retains the file's **last-good** index so navigation degrades gracefully.

Beyond navigation, the server also offers editing assistance:

- **formatting** — whole-document formatting and **range formatting** apply the same canonical
  layout as `ascript fmt`;
- **code actions** — quickfixes for individual diagnostics, **organize imports**, and a **fix-all**
  action that applies every available fix in the file at once;
- **completion** is **scope-aware**: it offers keywords, builtins, and the in-scope user bindings;
  on member access it completes the members of a class or enum and the exports of an imported module
  namespace; in an `import … from "…"` string it offers std module paths; it includes
  **control-flow snippets** and **auto-import** items that add the matching `import` statement for a
  known stdlib export, with `completionItem/resolve` filling in detail and documentation lazily.

Beyond the highlights above, the server answers the full modern LSP surface: **hover** with inferred
types, **signature help**, **semantic tokens** (full + range), **inlay hints**, **document
highlight**, **folding** and **selection ranges**, **document links**, **code lenses**, **call and
type hierarchy**, **document color** swatches, **linked editing**, **pull diagnostics**, multi-root
workspaces, and **rename-on-move** import rewriting. Editing stays responsive under load — rapid
keystrokes coalesce into one rebuild, stale completion/hover results are dropped, and very large
files degrade gracefully (`semanticTokens/full` goes range-only and inlay hints are skipped above
~256 KiB; `semanticTokens/full`/inlay/folding/color providers go quiet above ~2 MiB, though
`semanticTokens/range` is always served to keep the visible viewport colored) while diagnostics and
navigation always run.

See [editor setup](tooling/editor-setup) for VS Code, Zed, and Neovim configuration, and the
[LSP capability reference](tooling/lsp-capabilities) for every method the server answers.

> [!NOTE] The language server is **static-analysis only** — it lexes, parses, and resolves your
> source to produce diagnostics and navigation; it never runs the interpreter, so the whole layer
> stays `Send + Sync` and free of runtime state.
