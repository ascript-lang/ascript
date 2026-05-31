# Phase 2 — Program & CLI Toolkit Design

- **Date:** 2026-05-31
- **Status:** Design — proceeding under the standing multi-phase goal.
- **Roadmap:** Phase 2 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Make AScript a first-class language for writing command-line tools. Add the process
lifecycle + I/O + CLI ergonomics that a real `.as` program needs: set its exit status, read
its command line, read stdin, parse flags/subcommands, parse URLs/query strings, and colorize
terminal output. Builds on the just-shipped `std/log` (which already covers stderr) and live
`print` streaming.

## Sub-phase breakdown

Ordered additive-before-core within reason, dependencies respected:

- **2a — `exit(code)` + lifecycle plumbing** (core: new `Control::Exit`). The one
  architecturally significant piece.
- **2b — `env.args` (argv)** + thread trailing CLI args through clap (small, core-adjacent).
- **2c — `std/io`: stdin** (`io.readLine`, `io.readAll`, `io.readLines`) (new module, additive).
- **2d — `std/url`: URL + query-string parsing** (new module, additive).
- **2e — `std/cli`: argument parsing** (flags/options/subcommands/`--help`) (new module;
  depends on 2b `env.args`).
- **2f — `std/color`: ANSI styling** (new module, additive).

Each sub-phase is an independent spec→implement→review→commit unit; 2e depends on 2b.

---

## 2a — `exit(code)`

### Decision: unwind to the entry point, do NOT use `std::process::exit`

`exit(code)` raises a new control signal `Control::Exit(i32)` that unwinds the interpreter
stack up to the entry point (`run_file`), which returns the code; `main` maps it to
`ExitCode`. **Rationale:** unwinding runs `Drop` naturally — native resource handles (fds,
TCP, child processes) close, and structured-concurrency tasks abort on drop — whereas
`std::process::exit()` mid-stack skips all destructors. This matches the language's
"no hidden control flow / deterministic cleanup" design and the M17 cancel-on-drop model.

- **`exit` is a global builtin** (alongside `print`/`assert`/`len`/`recover`/…), because it
  is language-level control flow, not a stdlib domain function. Registered in `global_env()`
  and the builtin-name list; LSP keyword/builtin list updated.
- **Signature:** `exit(code = 0)`. `code` must be an integer in `0..=255` (POSIX exit range);
  non-integer or out-of-range is a Tier-2 panic. `exit()` with no arg → `0`.
- **`Control::Exit(i32)` is NOT catchable by `recover`** — `recover` only intercepts
  `Control::Panic`. `Exit` passes through `recover` untouched (like it passes through
  `try`/`?`), guaranteeing exit cannot be swallowed.
- **`Control` derives `Clone`** already (rides cross-task futures); `Exit(i32)` is trivially
  `Clone`. `AsError`→`Control::Panic` conversion is unaffected.
- **Plumbing:**
  - Add `Exit(i32)` to `enum Control` (`interp.rs:35`). Every place that matches `Control`
    exhaustively (the `?`/propagation sites, task boundaries, `run_*` entry points) gets an
    `Exit` arm that re-propagates (does not convert to error).
  - `run_file` (`lib.rs:30`) changes return type to `Result<i32, AsError>`:
    `Ok(0)` on normal completion / `Propagate`, `Ok(code)` on `Control::Exit(code)`,
    `Err(e)` on `Control::Panic`. `main.rs` `Run` arm: `Ok(code) => ExitCode::from(code as u8)`,
    `Err => {diagnostic; ExitCode::from(1)}`. `run_source`/`run_tests`/REPL get the analogous
    `Exit` arm (in the REPL, `exit()` ends the session with the code; in `run_tests`, an
    `Exit` during a test file is surfaced, not counted as pass).
  - **Output:** under `OutputSink::Live` (the `run` binary) output is already streamed, so no
    explicit flush needed; under `Capture` the entry point already returns the buffer.
  - **From a spawned task:** an `exit()` inside an awaited `async fn`/task propagates its
    `Control::Exit` across the task boundary when awaited (the task future already carries
    `Result<Value, Control>`). An `exit()` in a never-awaited detached task is delivered only
    if/when awaited — documented edge, consistent with how panics cross task boundaries.

### Tests
Interpreter unit tests: `exit(0)`/`exit(3)` set the code; `exit()` → 0; out-of-range/non-int
panics; `recover` does NOT catch exit; a `defer`-free resource (e.g. an open file handle via a
test double) is dropped on exit. Integration (`tests/cli.rs`): a `.as` program calling
`exit(2)` makes the binary exit 2; `exit(0)` → 0.

---

## 2b — `env.args` (argv)

- **`env.args()` → array of strings**: the arguments passed to the script *after* the file
  path. Added to `env::exports()` and `env::call`.
- **CLI threading:** `Run { file: String }` → `Run { file: String, args: Vec<String> }` with
  clap `#[arg(trailing_var_arg = true, allow_hyphen_values = true)]` so
  `ascript run prog.as --flag x y` passes `["--flag","x","y"]` to the script. `run_file` gains
  an `args: &[String]` parameter; `Interp` stores them (a `RefCell<Vec<String>>` or `Vec<Rc<str>>`
  cell) exposed via `env.args()`. `run_source`/REPL default to empty args.
- **Decision:** `env.args()` returns ONLY the script's own args (not the interpreter binary or
  the file path) — `process.run`-style scripts shouldn't have to skip `argv[0]`. A separate
  `env.scriptPath()` (the file being run) MAY be added if needed, but is out of scope unless
  trivial.

### Tests
`tests/cli.rs`: `ascript run args.as a b c` where `args.as` prints `env.args()` → `["a","b","c"]`.
Unit: a fresh `Interp` has empty `env.args()`.

---

## 2c — `std/io` (stdin)

New module `src/stdlib/io.rs`, registered in both `mod.rs` match arms, gated by the `sys`
feature (stdin is OS I/O; matches `fs`/`process`/`env`).

- `io.readLine() -> string | nil` — read one line from stdin (without the trailing `\n`);
  `nil` at EOF.
- `io.readAll() -> string` — read all of stdin to a string (UTF-8 lossy).
- `io.readLines() -> array<string>` — convenience: all lines as an array.
- Uses async `tokio::io::stdin` with the take-out-across-await resource pattern (never hold a
  `RefCell`/resource borrow across `.await`). Reads are line-buffered via a `BufReader`.
- **Decision:** stdin lives in `io`, not as a global `input()`, because it is domain I/O (and
  leaves room for `io.*` to grow). Prompting (`input(prompt)`) is sugar a script can write with
  `print` + `io.readLine`; not added as a primitive.

### Tests
`tests/cli.rs`: pipe stdin into a `.as` program that echoes `io.readLine()`/`io.readAll()` and
assert the output. (Spawns the binary with a piped stdin.)

---

## 2d — `std/url`

New module `src/stdlib/url.rs`, gated by `data` (parsing/serialization family). Use the
`url` crate (well-maintained, WHATWG-compliant) + manual query handling.

- `url.parse(s) -> [obj, err]` — Tier-1 result. On success an object:
  `{scheme, host, port, path, query, fragment, username, password}` (absent parts → `nil`;
  `query` is the raw query string). Parse failure → `err`.
- `url.parseQuery(s) -> object` — `"a=1&b=2"` → `{a:"1", b:"2"}` (repeated keys: last wins;
  document it). Percent-decoded.
- `url.buildQuery(obj) -> string` — object → percent-encoded `a=1&b=2` (insertion order).
- `url.build(obj) -> string` — assemble a URL from the component object shape above.
- `url.encode(s)` / `url.decode(s)` — percent-encode/decode a single component. (Note: overlaps
  `encoding.urlEncode`/`urlDecode`; `url.*` is component-correct — document the relationship,
  or have `url.encode` delegate to `encoding` to avoid divergence.)

### Tests
Round-trip parse/build; `parseQuery`/`buildQuery` round-trip; malformed URL → Tier-1 err;
unicode/percent cases. Plus an example.

---

## 2e — `std/cli` (argument parsing)

New module `src/stdlib/cli.rs`. Depends on 2b (reads `env.args()` by default). A declarative
parser configured by an AScript object spec; no proc-macros.

- `cli.parse(spec, args?) -> [result, err]` where `spec` describes flags/options/positionals:
  ```
  cli.parse({
    name: "mytool",
    flags:   [{ name: "verbose", short: "v", help: "..." }],          // boolean
    options: [{ name: "output", short: "o", default: "out", help: "..." }], // takes a value
    positionals: [{ name: "input", required: true }],
    subcommands: [ ... ],   // optional, recursive shape
  }, env.args())
  ```
  On success `result = { flags: {verbose:true}, options:{output:"x"}, positionals:{input:"f"}, subcommand: nil|{name, ...} }`.
- `--help`/`-h` → returns a generated help string in the result (or a dedicated
  `result.help`), without erroring; usage/errors on bad input come back as the Tier-1 `err`.
- **Decision:** pure-AScript-value config (no new syntax); errors are Tier-1 (`[result, err]`),
  not panics, because bad CLI input is an expected runtime condition. `--help` text is generated
  from the spec.
- Scope guard: support flags, value-options (with defaults), required/optional positionals,
  one level of subcommands, `--`-terminator, `--name=value` and `--name value`. NOT in scope:
  arg groups, mutually-exclusive sets, env-var fallbacks (future).

### Tests
Parse a representative spec over fabricated arg vectors: flags on/off, short/long, options with
defaults, missing required positional → err, subcommand dispatch, `--help` text generation,
`--` terminator. Example program.

---

## 2f — `std/color` (ANSI)

New module `src/stdlib/color.rs`. Lightweight, dependency-free (emit raw ANSI SGR codes).

- Foreground helpers: `color.red(s)`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`,
  `gray`/`grey`, `black` → wrap `s` in the SGR color + reset.
- Styles: `color.bold(s)`, `dim`, `italic`, `underline`.
- `color.rgb(r,g,b, s)` and `color.bg(...)` for truecolor/background (stretch; include if cheap).
- `color.strip(s)` — remove ANSI codes.
- **Decision:** functions wrap-and-reset (compose by nesting). Respect `NO_COLOR` env var: if
  set, all helpers return `s` unchanged (the de-facto standard). A `color.enabled(bool)` toggle
  MAY override. No automatic TTY detection in v1 (keep deterministic; document that callers can
  check themselves) — revisit.

### Tests
Each helper wraps with the right code + reset; `strip` removes them; `NO_COLOR` disables;
nesting composes.

---

## Cross-cutting requirements (every sub-phase)

- **No new language syntax** in Phase 2 — all features are builtins/stdlib modules, so
  tree-sitter/parser/formatter are structurally unaffected. EXCEPTION: `exit` is a global
  builtin name — add it to the LSP builtin/keyword completion list (not the grammar).
- New stdlib modules: create `src/stdlib/<m>.rs` with `exports()` + `call(...)`, register in
  BOTH `mod.rs` match arms, declare `pub mod` with the right `#[cfg(feature)]`, add example +
  docs page (`docs/content/stdlib/*.md`) + README stdlib table row.
- Clippy clean under `--all-targets` AND `--no-default-features --all-targets`.
- Examples are runnable and exercised by conformance tests; formatter idempotent on them.
- No deferrals/TODOs except where this spec explicitly scopes something out (noted inline).

## Feature-flag placement
- `io` → `sys`; `url` → `data`; `cli` → core (no feature gate — arg parsing is fundamental and
  dependency-light) OR `data` if it ends up needing a dep (decide at impl: prefer no new dep);
  `color` → core (no dep, tiny). `exit`/`env.args` → core.

## Open decisions (made; flagged for the record)
1. `exit` unwinds via `Control::Exit` (not `process::exit`) — see 2a rationale. **Settled.**
2. `exit` is a global builtin, not `os.exit`/`process.exit` (no `os` module until Phase 5).
   **Settled.**
3. stdin in `std/io`, not a global `input()`. **Settled.**
4. `url.encode` relationship to existing `encoding.urlEncode` — delegate to avoid divergence.
   **Settled (delegate).**
5. `cli` errors are Tier-1, config is plain AScript values. **Settled.**
6. `color` respects `NO_COLOR`, no auto-TTY detection in v1. **Settled.**
