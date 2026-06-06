# Phase 2 — Program & CLI Toolkit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Make AScript a first-class CLI language: `exit(code)`, `env.args`, stdin (`std/io`), URL parsing (`std/url`), arg parsing (`std/cli`), ANSI color (`std/color`).

**Architecture:** New control signal `Control::Exit(i32)` unwinds to the entry point (runs Drop). `env.args` threads CLI trailing args through clap → Interp. Three+ new stdlib modules follow the established pattern (`exports()` + `call()`, registered in `mod.rs`, feature-gated). No new language syntax.

**Conventions (from Phase 1):** helpers `arg/bi/want_string/want_number/want_array/want_object` in `mod.rs`; Tier-2 panic on type misuse; Tier-1 `[value, err]` for expected failures; never hold a `RefCell`/resource borrow across `.await` (take-out pattern); transforms return new values; tests inline (`#[test]`/`#[tokio::test]`) + `.as` examples exercised by conformance; clippy clean under `--all-targets` AND `--no-default-features --all-targets`; update `docs/content/stdlib/*.md` + README stdlib table per new module.

Each sub-phase below is its own implement→spec-review→quality-review→commit cycle. 2e depends on 2b.

---

## Sub-phase 2a: `exit(code)` via `Control::Exit`

**Files:** `src/interp.rs` (Control enum + global builtin + exhaustive matches), `src/lib.rs` (run_file/run_source/run_tests return code), `src/main.rs` (map to ExitCode), `src/repl.rs` (Exit ends session), `src/lsp/*` (add `exit` to builtin list), tests in `tests/cli.rs` + interp unit tests.

- [ ] **Step 1 (failing test):** `tests/cli.rs` — write a temp `.as` that calls `exit(2)`; assert the binary exits with code 2; another calling `exit(0)`→0; one calling `exit()` (no arg)→0. Also an interp unit test: a program `exit(3)` drives `run_source` to report code 3; `recover(fn(){ exit(5) })` still exits 5 (recover does NOT catch Exit).
- [ ] **Step 2:** run, verify fail.
- [ ] **Step 3 (implement):**
  - Add `Exit(i32)` to `enum Control` (`interp.rs:35`). Add `#[derive(Clone)]` already present — confirm `Exit` is Clone.
  - Add a global builtin `exit`: register `"exit"` in `global_env()` and in the builtin-name list (the array near `interp.rs:53` that lists `print`,`assert`,…). Implement its call: parse arg 0 (default `0`); require integer `0..=255` (else Tier-2 panic `"exit code must be an integer in 0..=255"`); return `Err(Control::Exit(code as i32))`.
  - Find EVERY exhaustive `match` on `Control` (eval/exec propagation, task boundaries in `task.rs`/`coro.rs`, `run_*`). For propagation sites, `Control::Exit` must bubble like `Propagate` (return early), NOT convert to a panic. For `recover` (which matches `Control::Panic`), ensure `Exit` falls through untouched (re-raised). Grep `Control::Panic` / `Control::Propagate` to find sites; add `Control::Exit(_)` arms that re-propagate.
  - `run_file` (`lib.rs:30`): return `Result<i32, AsError>`. Map `Ok(())`/`Err(Propagate)`→`Ok(0)`, `Err(Control::Exit(c))`→`Ok(c)`, `Err(Control::Panic(e))`→`Err(e)`. Same for `run_source`. `run_tests`: an `Exit` during loading surfaces (don't count as pass).
  - `main.rs` `Run` arm: `Ok(code) => ExitCode::from(code as u8)`.
  - `repl.rs`: on `Control::Exit(c)` end the REPL loop (optionally print nothing) — session ends.
  - LSP builtin/keyword completion list: add `exit`.
- [ ] **Step 4:** run tests + `cargo clippy --all-targets` + `--no-default-features --all-targets`. All green.
- [ ] **Step 5 (commit):** `feat(lang): exit(code) builtin via Control::Exit unwind`

> CRITICAL for the implementer: `exit` must run destructors — that is the whole point of unwinding instead of `std::process::exit`. Do NOT call `std::process::exit` anywhere. Verify by a test that a native resource is reclaimed (or at least that the unwind reaches `run_file`). If you discover a `Control` match you cannot resolve (e.g. a `From`/`?` site), report it rather than guessing.

---

## Sub-phase 2b: `env.args` (argv) + clap threading

**Files:** `src/main.rs` (Run command + call), `src/lib.rs` (run_file signature), `src/interp.rs` (store args), `src/stdlib/env.rs` (export + call), `tests/cli.rs`.

- [ ] **Step 1 (failing test):** `tests/cli.rs` — temp `.as` printing `env.args()`; run `ascript run <file> a b --x`; assert output is `["a", "b", "--x"]`. Unit: fresh `Interp` → `env.args()` is `[]`.
- [ ] **Step 2:** verify fail.
- [ ] **Step 3 (implement):**
  - `Run { file: String }` → `Run { file: String, args: Vec<String> }` with `#[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String>`.
  - `run_file(path, args: &[String])` — thread args in; store on `Interp` (a `RefCell<Vec<Rc<str>>>` or `Vec<String>` field set at construction or via a setter). `run_source`/`run_tests`/REPL pass `&[]`.
  - `env.rs`: add `("args", bi("env.args"))` to exports; `"args"` arm returns `Value::Array` of the stored strings. `env.rs` `call` is currently free `fn call(func,args,span)` with no `Interp` access — args need the interpreter. Route `env.args` through the interpreter like `array`: either (a) add `"args"` handling in the interpreter's env dispatch, or (b) if env is dispatched statically, add a minimal `impl Interp` path for `env` mirroring `call_object`/`call_array`. Choose the minimal correct wiring; confirm other `env.*` still work.
- [ ] **Step 4:** tests + clippy both configs.
- [ ] **Step 5 (commit):** `feat(env): env.args + thread CLI trailing args through clap`

---

## Sub-phase 2c: `std/io` (stdin)

**Files:** `src/stdlib/io.rs` (new), `src/stdlib/mod.rs` (register both arms + `#[cfg(feature="sys")] pub mod io;`), `Cargo` (none — tokio stdin already available), `tests/cli.rs`, example.

- [ ] **Step 1 (failing test):** `tests/cli.rs` — spawn the binary on a `.as` that does `print(io.readLine())` then `print(io.readAll())` with piped stdin `"hello\nrest of input"`; assert output. (Use the existing process-spawning test helper that pipes stdin; check how `tests/cli.rs` spawns with stdin.)
- [ ] **Step 2:** verify fail.
- [ ] **Step 3 (implement):** `io.rs` with `exports()` (`readLine`,`readAll`,`readLines`) and an `impl Interp` async dispatch (stdin is async + needs the resource discipline). Use `tokio::io::stdin()` with a `BufReader`; for `readLine` strip the trailing `\n`, return `Value::Nil` at EOF. TAKE the reader out across `.await` (no `RefCell`/resources borrow held across await) — follow the native-resource pattern in `net_tcp.rs`/`process.rs`. Register in `mod.rs` (both arms), gated `#[cfg(feature = "sys")]`.
- [ ] **Step 4:** tests + clippy both configs (confirm `--no-default-features` excludes `io` cleanly).
- [ ] **Step 5 (commit):** `feat(io): std/io stdin (readLine/readAll/readLines)`

---

## Sub-phase 2d: `std/url`

**Files:** `src/stdlib/url.rs` (new), `mod.rs` (register, `#[cfg(feature="data")]`), `Cargo.toml` (add `url` crate to the `data` feature), `tests`, example, `docs/content/stdlib/data.md` (or a url page).

- [ ] **Step 1 (failing test):** unit tests — `url.parse("https://u:p@host:8080/a/b?q=1#f")` returns object with the right components; `parseQuery("a=1&b=2")`→`{a:"1",b:"2"}`; `buildQuery({a:"1",b:"2"})` round-trips; malformed input → Tier-1 err (`[nil, err]`).
- [ ] **Step 2:** verify fail.
- [ ] **Step 3 (implement):** add `url = { version = "2", optional = true }` to deps + the `data` feature list. Implement `parse`(→`[obj,err]`), `parseQuery`, `buildQuery`, `build`, `encode`/`decode` (delegate `encode`/`decode` to the existing `encoding` percent helpers if equivalent, to avoid divergence — confirm behavior matches). Object shape per spec 2d. Register in `mod.rs` both arms, `#[cfg(feature="data")]`.
- [ ] **Step 4:** tests + clippy both configs.
- [ ] **Step 5 (commit):** `feat(url): std/url parse/build + query string`

---

## Sub-phase 2e: `std/cli` (arg parsing) — depends on 2b

**Files:** `src/stdlib/cli.rs` (new), `mod.rs` (register; core/no-feature if dependency-free), tests, example, docs.

- [ ] **Step 1 (failing test):** unit tests over fabricated arg vectors per spec 2e: boolean flag (long+short), value option with default, required positional missing→err, subcommand dispatch, `--name=value` and `--name value`, `--` terminator, `--help` produces help text in the result.
- [ ] **Step 2:** verify fail.
- [ ] **Step 3 (implement):** `cli.parse(spec, args?)` returning `[result, err]`. Parse the AScript object `spec` (flags/options/positionals/subcommands) into a Rust representation, then parse the args vector by hand (dependency-free; do NOT pull in clap for runtime parsing — it's macro/builder-oriented). Default `args` to `env.args()` when omitted (needs Interp access → `impl Interp` dispatch like io). Generate `--help` text from the spec. Tier-1 errors for bad input. Register in `mod.rs`.
- [ ] **Step 4:** tests + clippy both configs.
- [ ] **Step 5 (commit):** `feat(cli): std/cli declarative argument parser`

---

## Sub-phase 2f: `std/color` (ANSI)

**Files:** `src/stdlib/color.rs` (new), `mod.rs` (register; core/no-feature, no dep), tests, example, docs.

- [ ] **Step 1 (failing test):** unit tests — `color.red("x")` == `"\u{1b}[31mx\u{1b}[0m"`; `bold`/`underline` codes; `strip` removes ANSI; nesting composes; with `NO_COLOR` set, helpers return input unchanged (test via `std::env::set_var` guarded, or a `color.enabled(false)` toggle to avoid global env mutation in tests — prefer the toggle for test isolation).
- [ ] **Step 2:** verify fail.
- [ ] **Step 3 (implement):** SGR constants; fg helpers (red/green/yellow/blue/magenta/cyan/white/gray/black), styles (bold/dim/italic/underline), `rgb`/`bg` if cheap, `strip` (regex or manual ESC-`[`…`m` scan), `NO_COLOR` respect + optional `enabled` toggle stored on `Interp` or a module static (prefer Interp state for testability; if static, make the test use explicit codes and a separate NO_COLOR check). Register in `mod.rs`. No dependency.
- [ ] **Step 4:** tests + clippy both configs.
- [ ] **Step 5 (commit):** `feat(color): std/color ANSI styling (NO_COLOR aware)`

---

## Final integration (after 2a–2f)

- [ ] Create `examples/cli_toolkit.as` exercising exit (in a sub-path that doesn't abort the example — e.g. guarded), env.args, io (guarded/optional), url, cli, color end-to-end; run it.
- [ ] Update `docs/content/stdlib/*` pages + README stdlib table (new `io`/`url`/`cli`/`color` rows; `env.args`; document `exit`).
- [ ] FULL gates: `cargo test`, `cargo test --no-default-features`, `cargo clippy --all-targets`, `cargo clippy --no-default-features --all-targets`, `cargo fmt --check`, conformance (`treesitter_conformance`, `frontend_conformance`), formatter idempotence on new examples.
- [ ] Final holistic review, then merge `--no-ff` to main.

## Self-review notes
- Spec coverage: 2a exit, 2b args, 2c stdin, 2d url, 2e cli, 2f color — all mapped. No deferrals beyond the spec's explicit scope guards.
- Riskiest task is 2a (Control::Exit touches exhaustive matches) — implementer must find ALL `Control` match sites; the spec-reviewer must independently grep for missed sites.
- `env.args`/`io.readLine`/`cli.parse` need `Interp` access → `impl Interp` dispatch (mirror `call_array`/`call_object`); the static-`call` modules (url/color) don't.
