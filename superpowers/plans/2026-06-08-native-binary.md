# BIN — Native Single-Binary Distribution — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`.

**Spec:** `superpowers/specs/2026-06-08-native-binary-design.md`. **Branch:** `feat/native-binary` off
`main`. **Depends on:** **FUZZ** (hard gate, two conditions — see Lock pre-reqs) and **P0** (the `.aso`
reader allocation-clamp bugfix). **Breaking:** **no** — purely additive, CLI-side; no language/AST/opcode
change, **no `.aso` format change, no `ASO_FORMAT_VERSION` bump** (the verified `.aso` is embedded
verbatim).

**Architecture:** `ascript build --native app.as -o app` appends `payload || footer` to a copy of the
running runtime (`current_exe()`), where `payload` is the *verified* `.aso` bytes produced by reusing
`build_file`'s compile+verify+`to_bytes` front half (`lib.rs:356`, `:363`, `:366`, `:372`), and `footer`
is a fixed-size, magic-tagged trailing struct. On startup, `real_main` (`main.rs:210`) reads the footer of
`current_exe()` **before** `Cli::parse()` (`main.rs:211`); if present, it slices the payload and runs it
through the SAME verified path as `run file.aso` — `Chunk::from_bytes_verified` (`verify.rs:782`) → `Vm`
(`lib.rs:389`–`:444`) — forwarding argv (minus argv[0]) as the program's `env.args()`. This is **bundling,
not AOT**: the embedded VM still interprets. Worker-in-bundle works for free via `set_worker_aso_bytes`
(`interp.rs:852`), which `run_embedded_aso` sets exactly as `run_aso_file` does (`lib.rs:403`) — isolates
re-parse those already-verified bytes decode-only (`dispatch.rs:1319`), never re-exec (`isolate.rs:115`).
`--target <triple>` is **parsed-but-cleanly-rejected** in v1 (Tier-1 error). macOS-arm64 output is
**ad-hoc signed** post-append (`rcodesign`/`apple-codesign` crate, no Xcode) so the kernel will exec it.

**Tech stack:** Rust; `src/main.rs` (CLI + pre-clap startup shim), `src/lib.rs` (`build_native`,
`run_embedded_aso`), a new `src/bundle.rs` (footer codec + macOS ad-hoc sign), `clap` (the `Build`
subcommand flags), the `apple-codesign`/`rcodesign` crate (macOS sign). **No** grammar/parser/tree-sitter/
formatter/type-system/LSP/REPL/`.aso`-format work.

---

## Lock pre-reqs (BIN may NOT start until BOTH hold — §4, Gate 6)

BIN runs the `.aso` deserializer + verifier inside end-user-downloaded binaries over attacker-editable
bytes, so the trust boundary is distribution-grade. Per spec §4 and `goal.md` (P0 gates BIN; "Must land
after FUZZ hardens the `.aso` reader"):

1. **P0 reader allocation-clamp bugfix has MERGED.** Every attacker-controlled-length allocation in the
   `.aso` reader (`reserve`/`with_capacity` in `aso.rs` `read_chunk`/`read_proto`/`read_value`/`read_type`)
   is clamped with `.min(r.remaining())` — the pattern the worker serializer already uses
   (`serialize.rs:564`, `with_capacity(len.min(r.remaining()))`) — so a crafted huge-`u32`-length `.aso`
   yields a clean `AsoError`, NOT a SIGABRT(OOM) before `verify` runs. (TDD'd in P0; BIN consumes it.)
2. **FUZZ `.aso`/verifier target has met its sustained-clean bar — NOT merely one green CI run.** Per
   `2026-06-08-fuzzing-infra-design.md:11`–`:12`/`:313`–`:321`: **≥ 7 consecutive nightly runs of ≥ 4 h
   each** on the `aso_roundtrip` target with **zero crashes** since the last reader/verifier change. A
   single green per-PR CI run (a smoke re-run of known inputs) does NOT satisfy this; the deep nightly
   campaign is the gate.

A task in this plan (Task 0) records evidence that both hold before any code lands.

## Shared API Contract (pinned to current code)

**Reuse, unchanged (verified):**
- `build_file` (`lib.rs:356`) — compile (`compile_source`, `:363`) + verify (`vm::verify::verify`, `:366`)
  + `chunk.to_bytes()` (`:372`). `build_native` factors and reuses the front half (do NOT fork).
- `run_aso_file` (`lib.rs:389`) — reads bytes → `Chunk::from_bytes_verified` (`:396`) → `Interp::new_live`
  (`:399`) → `set_cli_args` (`:400`) → `set_worker_aso_bytes` (`:403`) → `install_self` (`:404`) →
  `Vm::new` (`:405`) → `LocalSet` run (`:425`–`:431`) → `gc::collect` (`:436`) → `RunOutcome`/`Control`
  exit-code map (`:437`–`:443`, incl. `Control::Exit(code)` `interp.rs:45`). `run_embedded_aso` factors
  the post-bytes body so the two share ONE impl.
- `Chunk::from_bytes_verified` (`verify.rs:782` = `from_bytes` + `verify`, returns
  `FromBytesVerifiedError` `verify.rs:792`). The single trust boundary; verified once at startup.
- `ASO_MAGIC = b"ASO\0"` (`aso.rs:50`), `ASO_FORMAT_VERSION = 18` (`aso.rs:105`, **read-and-compared on
  load, NEVER bumped** — `aso.rs:460`). `to_bytes` (`aso.rs:437`), `from_bytes` (`aso.rs:453`).
- Worker subsystem: `worker_aso_bytes` (`interp.rs:558`/`:852`/`:858`), `resolve_worker_top_chunk` decode-
  only re-parse via unverified `Chunk::from_bytes` over already-verified bytes (`dispatch.rs:1309`/`:1319`),
  in-thread fresh-`Vm` `bootstrap` — no re-exec (`isolate.rs:115`/`:197`). **No worker code change.**
- `Build` subcommand (`main.rs:36`, fields `file`/`out`); `real_main` (`main.rs:210`), `Cli::parse`
  (`main.rs:211`), the `.aso`-always-VM rule (`main.rs:230`), the `WORKER_STACK_SIZE` worker-thread main
  (`main.rs:139`–`:148`).
- pkg content-addressed cache (for the STAGED `--target` follow-up only, NOT v1): `cache_root` honoring
  `$ASCRIPT_CACHE` (`pkg/cache.rs:26`), `store_dir` (`:89`), staging (`:99`).

**New names (do not rename):**
- `src/bundle.rs`: `FOOTER_SIZE` (const), `BUNDLE_MAGIC = *b"ASCRIPTB"` (distinct from `ASO_MAGIC`),
  `BUNDLE_FOOTER_VERSION: u16 = 1`, `struct BundleFooter { payload_offset: u64, payload_len: u64,
  aso_version: u32, bundle_version: u16, reserved: u16, magic: [u8;8] }`, `write_footer(stub_len,
  payload_len) -> [u8; FOOTER_SIZE]`, `read_bundle_footer(exe_bytes: &[u8]) -> Option<(usize, usize)>`
  (returns bounds-checked `(payload_offset, payload_len)` or `None`), and `adhoc_sign_macos(path)`.
- `src/lib.rs`: `build_native(file: &Path, out: Option<&Path>, target: Option<&str>) -> Result<PathBuf,
  AsError>`, `run_embedded_aso(payload: &[u8], args: &[String]) -> Result<i32, AsError>`.
- `src/main.rs`: `Build { file, out, native: bool, target: Option<String> }`; a pre-clap
  `try_run_embedded() -> Option<ExitCode>` shim.

## Conventions (every task)

- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs
  (`--all-targets` and `--no-default-features --all-targets`).
- No `await` across a `RefCell`/resource borrow (Gate 4) — `run_embedded_aso` mirrors `run_aso_file`'s
  borrow discipline exactly.
- Tests spawn the built binary (`env!("CARGO_BIN_EXE_ascript")`, the `tests/cli.rs:11` posture).
- **No `.aso` format change, no `ASO_FORMAT_VERSION` bump** (spec §2, §11; Gate-1 four-mode parity stays
  free because the embedded payload is byte-identical to a `build` artifact).

---

## Task 0 — Lock-gate evidence (no code)
**Files:** none (record evidence in the PR description / `roadmap.md` BIN entry).
- [ ] Confirm and cite that **P0 has merged** (the `aso.rs` reader allocations are `.min(r.remaining())`-
  clamped; the crafted-huge-length test yields a clean `AsoError`). Run that test; paste output.
- [ ] Confirm the **FUZZ `aso_roundtrip` sustained-clean bar** is met (≥ 7 consecutive ≥ 4 h nightly
  crash-free runs since the last reader/verifier change — `fuzzing-infra-design.md:313`–`:321`). Link the
  nightly run records. **Do not proceed to Task 1 until both are recorded.**

## Task 1 — Footer codec + macOS ad-hoc sign (`src/bundle.rs`)
**Files:** `src/bundle.rs` (new), `src/lib.rs` (`pub(crate) mod bundle;`), `Cargo.toml`
(`apple-codesign`/`rcodesign` dep, macOS-gated). **Tests:** unit tests inline in `src/bundle.rs`.
- [ ] Failing unit tests: `write_footer` round-trips through `read_bundle_footer` (offset/len recovered);
  a non-bundle blob (no trailing `ASCRIPTB`) → `None`; a file shorter than `FOOTER_SIZE` → `None`; a
  footer whose `payload_offset + payload_len > exe_len - FOOTER_SIZE` → `None` (NO panic, NO OOB slice);
  `payload_offset` below the minimum stub size → `None`; `BUNDLE_MAGIC != ASO_MAGIC` (distinctness).
- [ ] Implement the fixed-size LE footer struct + `write_footer`/`read_bundle_footer` (the §2.1 layout,
  bounds-checked per §2.5: O(1) — read the last `FOOTER_SIZE` bytes, match magic, validate bounds, return
  `(offset, len)`). Implement `adhoc_sign_macos(path)` behind `#[cfg(target_os = "macos")]` using the
  `apple-codesign`/`rcodesign` crate (the `codesign -s -` equivalent — §10, owner decision "BIN sign via
  `rcodesign` crate"); a no-op stub on non-macOS.
- [ ] Green both configs; clippy. Independent review (greps for any unchecked slice / `unwrap` on the
  footer read; confirms the magic is `ASCRIPTB`, distinct from `ASO\0`). Commit.

## Task 2 — `run_embedded_aso`: factor the verified run body out of `run_aso_file`
**Files:** `src/lib.rs`. **Tests:** `tests/native.rs` (new) covers it end-to-end via Tasks 4–5; a focused
unit/integration assertion here that `run_aso_file` still passes its existing path.
- [ ] Extract the post-bytes body of `run_aso_file` (`lib.rs:396`–`:444`) into `run_embedded_aso(payload:
  &[u8], args) -> Result<i32, AsError>`: `Chunk::from_bytes_verified(payload)` (the SAME §2.4 trust
  boundary), `Interp::new_live` → `set_cli_args(args)` → `set_worker_aso_bytes(Rc::from(payload))`
  (`lib.rs:403`) → `install_self` → `Vm::new` → `LocalSet` run → `telemetry_flush_on_exit` →
  `gc::collect` → the `RunOutcome`/`Control` exit-code map (incl. `Control::Exit`). Set the module dir to
  `current_exe().parent()` for the embedded case (§2.4); `run_aso_file` keeps `path.parent()`.
- [ ] Rewrite `run_aso_file` as "read file → slice the whole file as payload → `run_embedded_aso(bytes,
  args)`" so the standalone-`.aso` and embedded paths share ONE impl (the verified load, worker bytes, run,
  exit-code mapping are now in one place). Keep the borrow discipline (no `RefCell`/resource borrow held
  across `.await` — Gate 4).
- [ ] Green both configs (existing `.aso` tests + `vm_differential.rs` unchanged — same path); clippy.
  Review (diff `run_aso_file` semantics: identical behavior, just refactored). Commit.

## Task 3 — `build_native` + CLI flags + startup shim
**Files:** `src/lib.rs` (`build_native`), `src/main.rs` (`Build` flags, `try_run_embedded` shim, error
mapping). **Tests:** `tests/native.rs` for the `--target`-rejected + `--target`-without-`--native` cases;
e2e in Tasks 4–6.
- [ ] **`build_native(file, out, target)` in `lib.rs`:** reuse `build_file`'s compile+verify+`to_bytes`
  front half (factor the shared part — §2.2 step 1; the payload is the SAME verified `.aso` byte vector).
  If `target.is_some()` → return the specific Tier-1 error (§3.2): *"cross-compilation is not yet supported
  (BIN v1 bundles for the host platform only). Build on a `<T>` host…"* mentioning the requested triple.
  Else: stub = `current_exe()` bytes; compute `payload_offset = stub.len()`; append `payload || footer`
  (`bundle::write_footer`); write to `out` (default = source stem with NO extension, or `<stem>.exe` on
  Windows — NOT `.aso`); `chmod +x` on Unix; on macOS call `bundle::adhoc_sign_macos(out)` (§2.2 step 5,
  mandatory on arm64). Print `bundled <file> -> <out> (<size>)`.
- [ ] **CLI (`main.rs:36`):** add `native: bool` and `target: Option<String>` to `Build`. Route `Build` to
  `build_native(..., target.as_deref())` when `native` is set, else the existing `build_file`
  (`main.rs:285`). `--target` WITHOUT `--native` → a usage error (clean message, non-zero exit). Map
  `build_native` errors to a clean message + exit 1, like `build_file` (`main.rs:290`).
- [ ] **Startup shim (`real_main`, `main.rs:210`):** at the very top, BEFORE `Cli::parse()` (`:211`), call
  `try_run_embedded()`: resolve `current_exe()` (a failure → treat as no-bundle, fall through — §2.3),
  read its tail via `bundle::read_bundle_footer`; if `Some((off, len))`, slice the payload, call
  `run_embedded_aso(payload, &argv[1..])`, map its result to `ExitCode`, and `return` — so a bundled
  binary NEVER reaches clap (argv is the program's, not ascript subcommands). A plain `ascript` → `None` →
  fall through, byte-identical to today.
- [ ] Failing tests: `ascript build --native --target x86_64-unknown-linux-gnu app.as` exits non-zero with
  the SPECIFIC cross-compile message naming the triple (NOT a generic clap error, NOT a silent ignore —
  Gate 6/10); `--target` without `--native` is a usage error.
- [ ] Green both configs; clippy. Review (confirm `--native` reuses `build_file`'s verify; confirm the
  shim runs before `Cli::parse` and that a non-bundle launch is unaffected). Commit.

## Task 4 — e2e equivalence (stdout + STDERR + exit; scrubbed PATH) — Gate 9
**Files:** `tests/native.rs` (new). **Tests:** spawn `env!("CARGO_BIN_EXE_ascript")`.
- [ ] For each representative example: (a) `build --native examples/<x>.as -o <tmp>/app`; (b) run
  `<tmp>/app` with a **scrubbed `PATH`** (no `ascript`) and a CWD with no `.as`/`.aso`; (c) assert
  **stdout, STDERR, and exit code all == `ascript run examples/<x>.as`** (stderr is load-bearing — §8/R4).
  The comparison is the *program's* output only, NOT ascript's `bundled … -> …` build chrome.
- [ ] Cover at least: a plain program (`examples/hello.as`); one reading `env.args()` (assert
  `./app a b --c` → the program sees `["a","b","--c"]`); a **stderr-emitting** program
  (`examples/logging.as` — `log.info`/`log.error` go to stderr, so the stderr channel is exercised, not
  vacuously equal); and a worker program (Task 6 deepens this).
- [ ] Green both configs; clippy. Review (confirm PATH is genuinely scrubbed and the stderr assertion is
  non-vacuous — the chosen example actually emits on stderr). Commit.

## Task 5 — Security: verifier rejects a tampered embedded chunk / corrupt footer — Gate 9
**Files:** `tests/native.rs`. **Tests:** spawn the built binary.
- [ ] Build a `--native` binary; locate the embedded `.aso` region via the footer offset; **flip a byte
  inside it**; run the binary → assert it **exits non-zero with a clean load/verify error**, NOT a
  panic/abort/SIGSEGV, NOT silent execution (this is the test that the FUZZ-hardened
  `from_bytes_verified` is the real gate — §2.5, §8).
- [ ] A second variant **corrupts the footer's `payload_offset`** (points past EOF) → assert the clean
  "not a valid bundle"/bounds error (the §2.5 bounds check → fall-through/clean error, never an OOB slice).
- [ ] Green both configs; clippy. Review (confirm the byte flip lands inside the `.aso` payload, not the
  footer; confirm no `abort`/signal). Commit.

## Task 6 — Worker-in-bundle parity — Gate 9, §7
**Files:** `tests/native.rs`. **Tests:** spawn the built binary.
- [ ] Pick a worker example (e.g. `examples/workers_parallel_map.as` — `worker fn` parallel map+gather).
  `build --native` it; run the bundle with scrubbed PATH; assert stdout+stderr+exit == `ascript run` of the
  same source. This proves isolates spawn and get their slice from `worker_aso_bytes` in embedded mode
  (decode-only re-parse of the already-verified payload — `dispatch.rs:1319`, §2.4/§7), with NO worker code
  change and NO re-exec (`isolate.rs:115`). Optionally vary `ASCRIPT_WORKERS` to confirm it still caps the
  pool in bundle mode (§2.3/§7).
- [ ] Green both configs; clippy. Review (confirm the worker program actually fans out; confirm no
  `worker_source` path is taken — there is no source in bundle mode). Commit.

## Task 7 — Startup-overhead benchmark (the non-bundle path) — Gate 12, §2.3/R2
**Files:** `bench/` (a script + recorded result, alongside `bench/run_workers_bench.sh`); record the number
in the PR / `roadmap.md`. **Tests:** measurement, not an assertion-in-`cargo test`.
- [ ] Benchmark the **non-bundle-path** cost the footer check adds to EVERY launch: a `current_exe()`
  resolve + open + seek-to-end + read of the last `FOOTER_SIZE` bytes. Use `hyperfine 'ascript --version'`
  (a do-nothing launch isolating pure startup) over **≥ 1000 runs** against a baseline `main` with the
  check removed, in BOTH feature configs, on the CI reference host.
- [ ] Record the wall-clock delta. **The number is the deliverable** (§2.3 — "negligible needs a number").
  Budget: **≤ 1 ms / ≤ 2 % of bare `--version` startup**. If it exceeds budget, move the check behind a
  cheaper guard (only when `argv[0]` is not the canonical `ascript`/`ascript.exe`, or only when no
  subcommand parses) BEFORE BIN locks. Note the recorded number (SP1 perf-trade posture).
- [ ] Review (confirm ≥ 1000 runs, both configs, the number is recorded). Commit.

## Task 8 — macOS-arm64 exec smoke (CI) — §10/R3
**Files:** `tests/native.rs` (the e2e tests double as this when run on the macOS-arm64 runner); CI matrix
note in `roadmap.md`. **Tests:** the Task-4 e2e equivalence test, executed on macOS-arm64.
- [ ] Confirm the Task-4 e2e build-then-execute test runs on the **macOS-arm64 CI runner** — without the
  Task-1 ad-hoc sign, the bundle is `SIGKILL`ed ("killed: 9") at launch and the equivalence test fails
  (§10: arm64 refuses to exec an unsigned Mach-O; appending the payload invalidated the stub's signature).
  This test is the regression catch for a dropped ad-hoc sign.
- [ ] A CI smoke: a tiny `examples/`-backed `build --native` + run on each release-matrix OS, asserting a
  self-contained binary runs there (§8 CI smoke). Review. Commit.

## Task 9 — Docs + roadmap
**Files:** `docs/content/language/modules-async.md` (add a "Native binaries" section — append to the
existing page, NAV unchanged per `app.js:25`), `README.md` (CLI table + single-binary deliverable note),
`CLAUDE.md` (one line under the `build → .aso; run .aso` pipeline note), `roadmap.md` (BIN status).
- [ ] "Native binaries" section: `ascript build --native`, what self-contained means (the whole runtime +
  bytecode), the **size note** (tens of MB — the runtime, not the program; §6), the **host-only `--target`
  limitation** (parsed-but-rejected; staged follow-up), the macOS ad-hoc-sign vs authenticity-notarization
  split (§10), and the explicit **"bundling, not AOT — same VM"** framing (§1). README CLI table gets
  `build --native`. CLAUDE.md gets a line: `build --native` appends the verified `.aso` + a trailing footer
  to a copy of the runtime; startup reads the footer of `current_exe()` and runs it via the same
  `from_bytes_verified` path as `run file.aso`. (If a NEW doc page were added, its slug MUST go in the
  `NAV` array — but v1 appends to `modules-async.md`, so NAV is unchanged.)
- [ ] Review (links resolve; size + `--target` limitation + bundling-not-AOT framing present). Commit.

## Done when

Every task checked behind an independent review. Both lock pre-reqs (P0 merged + FUZZ `.aso` sustained-
nightly-clean) recorded (Task 0). `cargo test` AND `cargo test --no-default-features` green; clippy clean
both configs. The e2e equivalence test passes on **stdout + stderr + exit** with a scrubbed PATH (Gate 9),
including a stderr-emitting program and a worker program; the tampered-chunk and corrupt-footer tests
reject cleanly (no abort); the `--target`-rejected + `--target`-without-`--native` tests pin the Tier-1
error (Gate 6/10); the macOS-arm64 runner executes the bundle (ad-hoc sign holds, §10). The Gate-12
non-bundle startup delta is **measured and recorded** within budget. `vm_differential.rs` unchanged
(four-mode parity is free — the embedded run is the identical `from_bytes_verified` → VM path). **No
`.aso` format change, no `ASO_FORMAT_VERSION` bump.** Merge `--no-ff` to `main` (BIN is a leaf — nothing
depends on it; rebase onto `Int`/`Float` after NUM merges).
```

## Open question

The staged `--target` follow-up (§3.2) reuses the pkg content-addressed cache for per-target runtime
artifacts, but BIN v1 only *rejects* `--target` — no cache code is touched here. The follow-up is its own
spec/plan; it is NOT in this plan's scope (recorded so it isn't mistaken for a gap).
