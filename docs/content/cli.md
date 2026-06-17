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

Trailing arguments after the file are forwarded to the script as `env.args()`. Hyphen-prefixed values
(e.g. `--flag`) are also captured, so `ascript run app.as -- --verbose 3` is not needed; just
`ascript run app.as --verbose 3` works.

### Capability flags

`run` accepts four flags that restrict the program's access to OS resources. The flags compose
additively (denial is monotone); any `[capabilities]` table in the nearest `ascript.toml` is also
applied (the CLI can only tighten further, never re-grant what the manifest denied). See
[Capabilities](stdlib/caps) for the full permission model.

| Flag | Effect |
|---|---|
| `--deny <CAP>` | Deny one or more capabilities for this run (comma-separated or repeatable). Names: `fs`, `net`, `process`, `ffi`, `env`. |
| `--sandbox` | Deny all five dangerous capabilities at once. Sugar for `--deny fs,net,process,ffi,env`. |
| `--deny-net=<MODE>` | Granular net carve-out: `external` (allow loopback/private, block public) or `all`. |
| `--deny-fs=<MODE>` | Granular fs carve-out: `write` (reads allowed, writes denied) or `all`. |

### Debugging and profiling flags

| Flag | Effect |
|---|---|
| `--inspect` | Run under the DAP debugger instead of normally. Starts a Debug Adapter Protocol server over stdio; an editor's DAP client drives breakpoints and inspection. See [Debugging & profiling](tooling/debugging-profiling). |
| `--profile <MODE>` | Run under the CPU profiler (v1: `cpu` is the only mode). Writes a profile artifact on exit; program output is byte-identical. Requires the `profile` feature (default-on). See [Debugging & profiling](tooling/debugging-profiling). |
| `-o <FILE>` / `--out <FILE>` | Profile output path (default `profile.json`, or `profile.txt` for the `collapsed` format). Only meaningful with `--profile`. |
| `--profile-hz <N>` | Profiler wall-clock sample rate in samples/second (default 1000, i.e. ~1 ms). Only meaningful with `--profile`. |
| `--profile-format <FMT>` | Profile artifact format: `speedscope` (default, opens at speedscope.app), `collapsed` (Brendan-Gregg folded stacks), or the golden-stable `deterministic-speedscope` / `deterministic-collapsed` variants (inline clock, no wall-clock thread). Only meaningful with `--profile`. |

### Package flag

`--locked` — resolve dependencies exactly from `ascript.lock` (no network). Fails on any drift,
missing lock, or integrity mismatch. For CI and air-gapped environments. See [Packages](packages).

### Performance flags

| Flag | Effect |
|---|---|
| `--elide` | Enable **contract elision** — drop statically-*proven* runtime type-contract checks (call arguments, annotated `let` initializers, declared returns) from the executed bytecode/AST. Behavior is byte-identical; only proven checks are removed. **Off by default on `run`** (the per-run proof collector adds a small startup cost that is over the measured budget for short programs). Equivalent to `ASCRIPT_ELIDE=1`. `ascript build --elide` is the cost-free surface (the elision is baked into the durable `.aso`). See [Type contracts → Annotations and performance](language/type-contracts). |
| `--no-elide` | Force contract elision **off** (the permanent kill switch; wins over `--elide`). Equivalent to `ASCRIPT_NO_ELIDE=1`. |
| `--no-cache` | Bypass the **compile cache** for this run — always parse/resolve/compile from source. Equivalent to `ASCRIPT_NO_COMPILE_CACHE=1`. The cache only ever applies to the plain `.as`-on-the-VM path: `.aso`, `--tree-walker`, `--inspect`, `--profile`, and explicit `--elide` runs are never cached regardless of this flag. See [`ascript cache`](#ascript-cache). |

## `ascript build`

Compile a `.as` program to a `.aso` bytecode file, then run the artifact with no compile step.

```text
ascript build program.as              # → program.aso
ascript build program.as -o out.aso   # choose the output path
ascript run program.aso               # run the compiled bytecode
```

`.aso` is a versioned, verified compilation cache — see [Compilation & runtime](runtime) for what it
is, when to use it, and why it is not a stable cross-version format.

### Build-specific flags

| Flag | Effect |
|---|---|
| `-o <FILE>` / `--out <FILE>` | Output path (defaults to `<stem>.aso`, or the bare `<stem>` executable with `--native`). |
| `--strip` | Omit the optional debug section (module source + per-function line/variable tables). The default includes debug info for use with `--inspect`. |
| `--native` | Produce a self-contained native executable instead of a `.aso` — the full runtime plus the compiled program appended to a copy of the running binary. Bundling, not AOT: the embedded VM still interprets. Host architecture only in v1. See [Bundles](language/bundles). |
| `--target <TRIPLE>` | Target triple for `--native` (requires `--native`). Host-only in v1 — accepted but rejected with a clear error. |
| `--compress` | zstd-compress the embedded payload of a `--native` bundle (requires `--native`). The stub transparently decompresses it at startup before running. Produces a smaller artifact; the footer is marked as an extended (version 2) bundle. An uncompressed `--native` build is byte-identical to before. See [Bundles](language/bundles). |
| `--tier <rt-core\|rt-local\|rt-net\|rt-full>` | Force the runtime stub **tier** for a `--native` bundle (requires `--native`) instead of automatic nearest-superset selection from the program's `std/*` imports. The tiers form a cumulative chain (`rt-core ⊂ rt-local ⊂ rt-net ⊂ rt-full`); a tier below the program's requirements is rejected with an error naming the missing features and the modules that demand them. See [Bundles](language/bundles). |
| `--report-json <PATH\|->` | Emit the canonical JSON **build report** for a `--native` bundle to `<PATH>` (or `-` for stdout) — the CI / reproducible-build hook (requires `--native`). Carries the chosen tier, required/stub/unused feature lists, payload and artifact sha256, and module counts; it contains no timestamps, so a rebuild of the same source produces byte-identical JSON. The human-readable report always prints to stderr regardless of this flag. See [Bundles](language/bundles). |
| `--elide` | Bake **contract elision** into the artifact — drop statically-proven runtime type-contract checks from the compiled `.aso`/native bytecode. The win is durable (every later `run` of the artifact keeps it) and the one-shot collector cost is amortised, so this is the recommended elide surface. Behavior is byte-identical. Equivalent to `ASCRIPT_ELIDE=1`. Default-off; `--no-elide` forces it off. See [Type contracts → Annotations and performance](language/type-contracts). |
| `--no-elide` | Force contract elision off in the artifact (kill switch; wins over `--elide`). Equivalent to `ASCRIPT_NO_ELIDE=1`. |
| `--pgo` | **Profile-guided optimisation harvest** (WARM B §3.1). Run the program as a training workload, collect the VM's warmed inline caches and adaptive arithmetic state, and embed a compact PGO section into the produced `.aso`. The artifact is always an `ASCRIPTA` archive (even for a single-module program). Training-run stdout streams live. A panicking training run still produces a (possibly partial) section — the build never aborts on a training panic. |

The four capability flags (`--deny`, `--sandbox`, `--deny-net`, `--deny-fs`) are also accepted on
`build` and on `build --native`. The composed capability set is **embedded** in the produced
artifact and enforced at launch — you can further restrict it at run time with `ASCRIPT_DENY`, but
you can never widen it past what was baked in. See [Capabilities](stdlib/caps).

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

## `ascript doc`

Generate API documentation from your `///` and `//!` doc-comments — a self-contained HTML site
(default) or Markdown — with no external toolchain.

```text
ascript doc                       # document the current project (HTML → target/doc/)
ascript doc lib.as --format md    # Markdown to stdout
ascript doc --check               # CI gate: exit non-zero if any public symbol is undocumented
```

`///` documents the item below it (`fn`/`class`/`enum`/field/method); `//!` documents the module.
Bodies are Markdown. By default only the exported public API is documented (`--private` includes
the rest). See the [doc-generation reference](tooling/doc-generation) for the full convention,
formats, and the `--check` documentation gate.

Additional `doc` flags:

| Flag | Effect |
|---|---|
| `--out <DIR>` | Output directory (default `target/doc/`). |
| `--format <FMT>` | Output format: `html` (default, a self-contained directory tree) or `md` (Markdown to stdout or `--out`). |
| `--private` | Include non-exported declarations alongside the public API. |
| `--open` | Open the generated `index.html` in the default browser after writing (best-effort; requires the `sys` feature). |
| `--check` | Write nothing; exit non-zero if any public declaration lacks a doc-comment. |

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

### Test runner flags

| Flag | Effect |
|---|---|
| `--parallel[=N]` | Run each test file in its own shared-nothing worker isolate, in parallel. Bare `--parallel` uses `num_cpus` isolates; `--parallel=N` caps at N (further clamped by `$ASCRIPT_WORKERS`). Absent = serial (the default). A single file degrades to the serial path. The aggregated result and exit code are deterministic regardless of completion order. |
| `--coverage[=FORMAT]` | Record line coverage for the test run on the bytecode VM and emit a report. Bare `--coverage` prints a text summary; `--coverage=lcov` emits LCOV; `--coverage=html` writes a self-contained `target/coverage/` tree. Coverage is VM-only (the tree-walker is the oracle, not instrumented). Program output is byte-identical. |
| `--watch` | Re-run the affected tests on file change, scoped by the workspace import graph (only files whose import closure touched the changed file re-run). Runs until interrupted (Ctrl-C). Requires the `sys` feature (file watching). |
| `--filter <PATTERN>` | Run only tests whose name matches PATTERN — a substring by default, or a regex when written `/regex/`. Prunes which registered tests run and (when no test in a file matches) which files contribute. A skipped test is reported as "filtered", never pass/fail. A malformed regex is a clean error. |
| `--update-snapshots` | Re-baseline all snapshots this run (`jest -u`-style): a changed `assert.snapshot` value overwrites the stored baseline and **passes**, and orphan snapshot files (no matching assertion this run) are deleted. Without the flag a changed snapshot fails and orphans are only reported. |
| `--locked` | Resolve dependencies exactly from `ascript.lock` (no network). See [Packages](packages). |
| `--deny <CAP>` | Deny capabilities for the test run (same names as `run --deny`). |
| `--sandbox` | Deny all five dangerous capabilities for the test run. |
| `--elide` | Enable contract elision for the (serial) test run (default-off; behavior byte-identical). Equivalent to `ASCRIPT_ELIDE=1`. The `--parallel` path runs each file in a worker isolate, which never elides. |
| `--no-elide` | Force contract elision off (kill switch). Equivalent to `ASCRIPT_NO_ELIDE=1`. |

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
- **completion** is **frame-precise**: it offers keywords, builtins, module-globals, and exactly the
  local/parameter/closure bindings live at the cursor's frame (not sibling scopes); on member access it
  completes the fields and methods of the receiver's inferred class, the members of a class or enum, and
  the exports of an imported module namespace; in an `import … from "…"` string it offers std module
  paths; it includes
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

`--stdio` is accepted for compatibility with LSP clients that pass it (e.g. some VS Code configs).
stdio is the only transport, so the flag is a no-op.

See [editor setup](tooling/editor-setup) for VS Code, Zed, and Neovim configuration, and the
[LSP capability reference](tooling/lsp-capabilities) for every method the server answers.

> [!NOTE] The language server is **static-analysis only** — it lexes, parses, and resolves your
> source to produce diagnostics and navigation; it never runs the interpreter, so the whole layer
> stays `Send + Sync` and free of runtime state.

## `ascript cache`

Manage the compile cache. On `ascript run <file>.as` (the default VM path) the CLI consults this
cache before compiling: an unchanged program is loaded from its cached, verified bytecode instead
of being re-parsed, re-resolved, and re-compiled. The cached and uncached runs are **byte-identical**
(stdout, stderr, exit code, and panic carets alike). Bypass the cache with `--no-cache` or
`ASCRIPT_NO_COMPILE_CACHE=1`. It never applies to `.aso` (already compiled), `--tree-walker`,
`--inspect`, `--profile`, `--elide`, or `ascript test`.

The compile cache lives under the cache root (see `ascript cache dir`)
in a `compiled/` subdirectory. Each slot is a content-addressed directory keyed by a hash of
the compiler version, entry path, codegen flags, and resolved package map — not a hash of the
source. Source integrity is validated per-slot via a manifest of per-file digests over the whole
reachable import graph, so editing **any** module (entry, transitive, or a `{path=…}` dependency)
misses and recompiles, while a content-preserving `touch` (mtime-only change) still hits.

The cache is fail-open: any IO error, digest mismatch, hostile entry, or missing slot falls through
to a fresh compile without error — a normal `run` never fails because of the cache. Corruption in a
slot is self-healing: the verifier rejects it, the slot is deleted, and the next `ascript run`
recompiles and republishes.

### `ascript cache clean`

Remove the `compiled/` namespace entirely (all compile cache entries). The pkg `store/` namespace
(package tarballs) is **not** affected.

```text
ascript cache clean
```

Prints the number of slots removed, or a message if the cache was already empty. Use this to free
disk space or force a full recompile of all programs.

### `ascript cache dir`

Print the cache root directory.

```text
ascript cache dir
```

The cache root is resolved from `$ASCRIPT_CACHE` (if set and non-empty), then the per-platform
default (`~/Library/Caches/ascript` on macOS, `$XDG_CACHE_HOME/ascript` or `~/.cache/ascript`
on Linux, `%LOCALAPPDATA%\ascript\Cache` on Windows). Set `$ASCRIPT_CACHE` to redirect the cache
to a custom location (useful in CI or sandboxed environments).

## `ascript dap`

Run a standalone Debug Adapter Protocol server over stdio. An editor's DAP client connects to the
process and drives `launch`, breakpoints, stepping, and inspection. The program to debug comes from
the editor's `launch` request.

```text
ascript dap
```

`--stdio` is accepted for compatibility with DAP clients that pass it; stdio is the only transport,
so it is a no-op.

`ascript dap` takes no capability sandbox flags — the program path is not known at server start. If
you need a sandboxed debug session, use `ascript run --inspect --sandbox <file>` instead: that path
pre-sets the program AND composes the capability set before the DAP server starts (the same flags
that restrict a normal run restrict the debugged run).

For quick in-editor setup, use `ascript run --inspect <file>` to pre-set the program from the CLI.
See [Debugging & profiling](tooling/debugging-profiling) for the full setup guide and VS Code
launch configuration.

## Package management

When a project has an `ascript.toml` with `[dependencies]`, use the `ascript` package subcommands
to manage them. See [Packages](packages) for the full workflow, manifest format, and lockfile
semantics.

| Subcommand | Effect |
|---|---|
| `ascript add <SPEC>` | Add a dependency to `ascript.toml` and update the lock. The spec selects the source: a git URL with optional tag/rev, an archive URL, or a local path. |
| `ascript remove <NAME>` | Remove a named dependency from `ascript.toml` and re-lock. |
| `ascript install [--locked]` | Resolve + fetch all dependencies and write/verify `ascript.lock`. `--locked` installs exactly from the existing lock (no network); fails on any drift. |
| `ascript update [NAME]` | Raise version pins and re-lock. Pass a name to update a single dependency; omit to update all. |
| `ascript lock` | (Re)generate `ascript.lock` from the manifest without fetching. |
| `ascript tree` | Print the resolved dependency graph. |
| `ascript verify` | Re-hash the cache store against the lock integrity records; fails closed on any mismatch. |

## Environment variables

The runtime and CLI read a small set of `ASCRIPT_*` environment variables. These are debugging,
configuration, and CI knobs — most programs never need them.

| Variable | Effect |
|---|---|
| `ASCRIPT_CACHE` | Override the cache root for the package store (default: a platform-specific user cache directory). See [Packages](packages). |
| `ASCRIPT_DENY` | Comma-separated capability deny list applied to every embedded-binary run (equivalent to embedding `--deny` in a native bundle). For runtime sandbox enforcement. See [Capabilities](stdlib/caps). |
| `ASCRIPT_ELIDE` | Set to `1` to enable contract elision (drop statically-proven runtime type-contract checks), equivalent to the `--elide` flag on `run`/`build`/`test`. Off by default — elision is invisible to behavior; only proven checks are removed. See [Type contracts → Annotations and performance](language/type-contracts). |
| `ASCRIPT_ELIDE_PARANOID` | Set to `1` to enable **paranoid proof-violation mode** (ELIDE §6.3): all runtime type-contract checks are *retained* (elision is fully off), but any failure at a statically-proven site escalates to a `ELIDE proof violated (checker soundness bug): …` panic instead of the normal one. A diagnostic tool for detecting checker unsoundness bugs — healthy programs produce byte-identical output. Off by default. |
| `ASCRIPT_ENGINE` | Set to `tree-walker` to select the legacy tree-walker engine for `run` and `repl` instead of the bytecode VM. The `--tree-walker` flag takes precedence. Primarily a debugging and differential-oracle knob. |
| `ASCRIPT_LOG` | Log level for `std/log` output (`debug`, `info`, `warn`, `error`). Sets the filter threshold; messages below the level are dropped before any formatting. See [log](stdlib/log). |
| `ASCRIPT_DECODE_THRESHOLD` | Override the DECODE warmth threshold (default: 8). A proto must be entered at least this many times before its bytecode is decoded into the fixed-width record stream. Set to `0` to force immediate decoding. A benchmarking knob for threshold A/B runs — see the DECODE performance docs. |
| `ASCRIPT_NO_CALL_FAST` | Set to `1` to disable the CALL fast-path optimizations (in-place arg binding, fiber pooling, and the higher-order callback trampoline). Behavior is byte-identical to the default; only allocation counts and throughput differ. A debugging and benchmarking knob — equivalent to `--no-specialize` for the call path. |
| `ASCRIPT_NO_COMPILE_CACHE` | Set to `1` to bypass the compile cache for every `ascript run` (always parse/resolve/compile from source), equivalent to the `--no-cache` flag. The cache is invisible to behavior — cached and uncached runs are byte-identical — so this is a debugging / measurement knob. See [`ascript cache`](#ascript-cache). |
| `ASCRIPT_NO_DECODE` | Set to `1` to disable the DECODE optimisation (lazy decoded-dispatch record streams). The VM always executes directly from the bytecode stream. Behavior is byte-identical; a debugging and benchmarking knob. |
| `ASCRIPT_NO_DECODE_INLINE` | Inert. DECODE Unit C (speculative global-fn inlining) was evidence-dropped and never shipped; this switch is still parsed but has no effect. Retained only for tooling parity. |
| `ASCRIPT_NO_DECODE_TOS` | Inert. DECODE Unit D (top-of-stack register caching) was evidence-dropped and never shipped; this switch is still parsed but has no effect. Retained only for tooling parity. |
| `ASCRIPT_NO_ELIDE` | Set to `1` to force contract elision off (the permanent kill switch; wins over `ASCRIPT_ELIDE` / `--elide`). Redundant while elision is already off by default, but stable for when the default flips. |
| `ASCRIPT_NO_PGO` | Set to `1` to disable PGO warm-state seeding when loading a `build --pgo` archive. The program warms its inline caches / adaptive-arith sites from scratch instead of pre-installing the recorded profile. Behavior is byte-identical to the default (seeds only ever pre-warm caches that would warm anyway, behind the same runtime guards); a debugging and benchmarking knob. |
| `ASCRIPT_NO_SPECIALIZE` | Set to `1` to disable every VM specialization (field/method inline caches, adaptive arithmetic, the global cache). Behavior is byte-identical to the default; only speed differs. Useful for isolating a performance regression or verifying that the generic and specialized paths agree. |
| `ASCRIPT_NO_SYNC_LANE` | Set to `1` to disable the two-lane fiber engine's synchronous fast lane. The VM falls back to the async driver for every burst. Behavior is byte-identical; a debugging knob for isolating lane-related issues. |
| `ASCRIPT_RT` | **Build-time** (not a runtime knob): set to `1` when invoking `cargo build --bin ascript-rt` to compile the runtime-only stub — the front-end (parsers, compiler, checker, LSP/DAP/formatter/REPL/package-manager, tree-sitter) is excluded so the binary carries only the VM, GC, stdlib, workers, capabilities, and the `.aso`/archive loader. Unset for a normal toolchain build (which is byte-identical to before this flag existed). Used by `scripts/build-rt.sh`. |
| `ASCRIPT_RT_BASE_URL` | Override the base URL the builder fetches the `ascript-rt` stub release manifest and blobs from (default: the GitHub releases download URL). Supports `file://` for air-gapped mirrors and tests. **Moves the bytes, not the trust** — the same compiled-in ed25519 key still verifies the same signed manifest (RT §5.2). |
| `ASCRIPT_RT_NO_FETCH` | Set to `1` to skip the stub-fetch rung entirely (equivalent to `--no-fetch`): the builder never contacts the release host and falls through to the local dev fallbacks (`--stub`, a sibling `ascript-rt`, then `current_exe()`). An availability fall-through, never an integrity bypass (RT §5.2). |
| `ASCRIPT_RT_TIER` | **Build-time:** the tier label (`rt-core`/`rt-local`/`rt-net`/`rt-full`/`custom`) stamped into an `ascript-rt` stub at compile time and reported by `ascript-rt --rt-info`. Defaults to `custom`. Set by `scripts/build-rt.sh`; has no effect on a normal toolchain build. |
| `ASCRIPT_UPDATE_SNAPSHOTS` | Set to `1` to re-baseline all `assert.snapshot` calls, equivalent to `ascript test --update-snapshots`. Useful in CI scripts that want to update snapshots unconditionally. See [assert](stdlib/assert). |
| `ASCRIPT_WORKERS` | Maximum number of worker isolates for the pooled `worker fn` pool and `ascript test --parallel`. Defaults to `num_cpus`. See [Workers & parallelism](../language/workers). |
