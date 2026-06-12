# Runtime-Only Native Stubs (RT) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. A final **holistic
> review** covers the whole branch before merge. A task is closed only when every box under it
> is ticked.

**Goal:** `build --native` stops shipping the 42 MB toolchain as the runtime. A new
runtime-only `ascript-rt` bin target (VM/GC/stdlib/workers/caps/loaders/diagnostics/shim;
parsers, compiler, checker, LSP/DAP/fmt/REPL/pkg/tree-sitter compiled out) is published as a
prebuilt per-target **tier matrix** (rt-core ⊂ rt-local ⊂ rt-net ⊂ rt-full), fetched
fail-closed against an ed25519-signed, version-locked manifest into the content-addressed
`$ASCRIPT_CACHE` store, and selected by the **archive's own import facts** through a
drift-tested module→feature table. Plus: `--target` cross builds (platform-independent
payload onto pre-signed platform stubs), `--exact` local-cargo stubs, `--compress` (zstd
payload via footer flags), `--oci` (loadable OCI image tarball, no Docker), reproducible
outputs, and a `--report-json` build report. **v2 upfront — no minimal-v1 staging (owner
decision).**

**Architecture:** Spec: `superpowers/specs/2026-06-12-native-runtime-stubs-design.md` —
**read it fully before any task**; §2.3's residual source-parsing audit and §7.2's footer
strictness matrix are load-bearing. The frontend gate is a **build-time cfg**
(`ASCRIPT_RT=1` env → `cargo:rustc-cfg=ascript_rt` from `build.rs`, the `fuzzing`-cfg
precedent), NOT a Cargo feature (spec §2.2 — features are additive and
`--no-default-features` must keep building the parsers). Engines untouched:
`ASO_FORMAT_VERSION` stays 27, `ARCHIVE_VERSION` stays 1; the only wire change is
`BundleFooter.reserved → flags` with `bundle_version` 2 written ONLY for nonzero flags.

**Tech stack:** Rust, two bin targets of one crate. New CLI-side module tree
`src/rtstub/{mod,std_features,tiers,select,manifest,fetch,cache,oci,report}.rs` (all
`#[cfg(not(ascript_rt))]`; fetch/verify behind a new default-on `rt-fetch = ["net",
"dep:ed25519-dalek"]` feature; `bundle-zstd = ["dep:zstd"]` new default-on feature for the
stub-side decompressor). New bin `src/bin/ascript-rt.rs`. Touched: `build.rs`,
`Cargo.toml`, `src/bundle.rs`, `src/lib.rs`, `src/main.rs`, `src/vm/run.rs` (one cfg-gated
arm), `src/worker/dispatch.rs` (cfg-gated arms), `tests/native.rs`, new
`tests/{rt_stub,rt_supply_chain,rt_oci,rt_select}.rs`, `scripts/{build-rt.sh,
release-rt-stubs.sh}`, `.github/workflows/release-rt.yml`, `bench/RT_SIZE_RESULTS.md`,
`docs/content/{cli.md,language/bundles.md}`.

**Binding execution standards (non-negotiable):**
- TDD per task: failing test → minimal code → green → commit. Frequent commits, house
  trailer on every commit: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Production-grade mandate (goal.md Gates 1–14, goal-perf 15–18 where applicable):** any
  bug found while working — ours or pre-existing, direct or incidental — is fixed **in this
  branch** with a failing-test-first regression guard. No placeholders, no silent
  deferrals; every spec-§11 rejection stays rejected; every recorded future item lands in
  `roadmap.md` with an owner note.
- **Fail-closed is proven, not asserted:** every integrity check in §5/§7 has a negative
  test that demonstrates refusal; the reviewer sabotage-tests at least one per task.
- Clippy clean AND tests green under `--all-targets` and `--no-default-features
  --all-targets` before any "done" claim — AND (new for RT) the stub build
  (`scripts/build-rt.sh rt-core`) compiles clean from Task 1 on. Evidence (command output)
  before assertions.
- Untrusted bytes are hostile: footers, manifests, fetched stubs, cached stubs, compressed
  payloads — every length bounds-checked, no reachable `unwrap`/`panic!`, refusal messages
  name the reason.
- No `RefCell`/resource borrow across `.await`; no env reads in parallel tests (thread
  explicit flags/paths through `#[doc(hidden)]` seams; `$ASCRIPT_CACHE` per-test tempdirs
  follow the `pkg::cache::TEST_ENV_LOCK` discipline).
- Branch: `feat/native-runtime-stubs` off `main`. Merge `--no-ff` after holistic review.

---

## Task 0: Phase-0 measurement + behavior pins

Measure before cutting; pin the contracts RT changes or leans on.

**Files:** `bench/rt_size_matrix.sh` (new), `bench/RT_SIZE_RESULTS.md` (new),
`src/bundle.rs` (tests only), `tests/native.rs` (read only)

- [ ] **Step 1: Pin the footer baseline** (in `src/bundle.rs` `mod tests` — these pass
  TODAY and become the §7.2 contract's "before" anchor):

```rust
/// RT §7.2 baseline pin: the SHIPPED reader accepts any bundle_version and any
/// reserved value (only length + magic are checked). Task 3 tightens this for NEW
/// readers; this test documents what every already-shipped stub does with a v2
/// footer (payload magic check catches it downstream — an error, not a misread).
#[test]
fn shipped_reader_ignores_version_and_reserved() {
    let mut f = BundleFooter {
        payload_offset: MIN_STUB_SIZE, payload_len: 8, aso_version: AV,
        bundle_version: 99, reserved: 0xFFFF, magic: BUNDLE_MAGIC,
    };
    assert!(BundleFooter::from_bytes(&f.to_bytes()).is_some());
    f.reserved = 0;
    assert!(BundleFooter::from_bytes(&f.to_bytes()).is_some());
}

/// RT §6.1: writers have only ever emitted bundle_version=1, reserved=0 — the fact
/// that lets Task 3 define `v1 ⇒ flags must be 0` as corruption-detection.
#[test]
fn write_footer_emits_version1_reserved0() {
    let b = write_footer(MIN_STUB_SIZE, 4, AV);
    let f = BundleFooter::from_bytes(&b).unwrap();
    assert_eq!((f.bundle_version, f.reserved), (BUNDLE_FOOTER_VERSION, 0));
    assert_eq!(BUNDLE_FOOTER_VERSION, 1);
}
```

- [ ] **Step 2: Run — expect PASS** (pins existing behavior; a failure means the spec's
  ground truth is wrong — stop and escalate).
- [ ] **Step 3: Size matrix.** Write `bench/rt_size_matrix.sh`: `cargo build --release`
  (full default) → record binary size + `cargo bloat --release -n 40` (install if absent;
  fall back to `size`/section analysis and say so in the report); then per-feature deltas:
  for each runtime feature F in spec §3.1, `cargo build --release --no-default-features
  --features shared,F` size. Record machine/date/toolchain. (The per-TIER `ascript-rt`
  rows are appended by Task 2 once the bin exists — leave a marked section.)
- [ ] **Step 4:** Start `bench/RT_SIZE_RESULTS.md`: the full-binary baseline, the
  per-feature table, and an explicit "toolchain share" estimate (sum of bloat rows for
  `lsp`/`syntax`/`compile`/`check`/tree-sitter/clap/rustyline symbols) — the number the
  spec's motivation claims trace to.
- [ ] **Step 5: Commit** — `bench(rt): Phase-0 size matrix + footer baseline pins (RT Task 0)`.
- [ ] **Reviewer checkpoint:** reviewer re-runs the script, confirms numbers within noise,
  confirms NO production code changed, and that both pin tests pass on the branch point.

## Task 1: the `ascript_rt` cfg + the `ascript-rt` bin + the source-parsing gates

**Files:** `build.rs`, `Cargo.toml`, `src/bin/ascript-rt.rs` (new), `src/lib.rs`,
`src/main.rs`, `src/vm/run.rs`, `src/worker/dispatch.rs`, `scripts/build-rt.sh` (new)

- [ ] **Step 1: Write the failing checks first.** (a) A new unit test module
  `src/rt_gate_tests.rs`-style is impossible for a cfg we don't set under test — so the
  gate's tests are BUILD commands, scripted: extend `scripts/build-rt.sh`:

```bash
#!/usr/bin/env bash
# Build a runtime-only stub. Usage: scripts/build-rt.sh <rt-core|rt-local|rt-net|rt-full> [--target T]
set -euo pipefail
TIER="$1"; shift || true
case "$TIER" in
  rt-core)  FEATURES="shared,bundle-zstd" ;;
  rt-local) FEATURES="shared,bundle-zstd,data,binary,log,workflow,datetime,crypto,compress,sys,sysinfo,sql,tui" ;;
  rt-net)   FEATURES="<rt-local>,net,postgres,redis,telemetry" ;;   # expand literally
  rt-full)  FEATURES="<rt-net>,intl,ai,ffi" ;;
  *) echo "unknown tier '$TIER'" >&2; exit 2 ;;
esac
ASCRIPT_RT=1 ASCRIPT_RT_TIER="$TIER" cargo build --release --bin ascript-rt \
  --no-default-features --features "$FEATURES" "$@"
```

  (b) In `tests/native.rs`, a test that the NORMAL build is untouched: assert
  `Command::new(bin()).args(["build","--help"])` still lists today's flags (guards against
  accidental clap drift from main.rs surgery). Run `scripts/build-rt.sh rt-core` — expect
  FAIL (no bin target, no cfg).
- [ ] **Step 2: `build.rs` + `Cargo.toml`.** Add the env→cfg emission +
  `cargo:rerun-if-env-changed=ASCRIPT_RT` + skip the tree-sitter `cc` compile under it
  (spec §2.2). Register `cfg(ascript_rt)` in `[lints.rust] check-cfg` beside `fuzzing`.
  Add `[[bin]] ascript-rt` (path `src/bin/ascript-rt.rs`), the `bundle-zstd =
  ["dep:zstd"]` feature (+ into `default`), and stamp
  `println!("cargo:rustc-env=ASCRIPT_RT_TIER=…")` from the env (default `"custom"`).
- [ ] **Step 3: Gate the frontend.** `#[cfg(not(ascript_rt))]` on: `src/syntax/`,
  `src/compile/`, `src/parser.rs`, `src/check/`, `src/fmt.rs`, `src/repl.rs`, the
  lexer's parser-facing entries, and the toolchain entry points in `src/lib.rs`
  (`run_file*`, `run_source*`, `run_tests`, `build_file`, `build_native`,
  `compile_archive*`, the REPL/test seams). `src/main.rs`: whole-file gate with a tiny
  `#[cfg(ascript_rt)] fn main()` loud-error stub. Work the dependency fan-out
  honestly — every `use crate::compile::…` from runtime modules must be either already
  feature-gated, cfg-gated here, or one of the two audited sites in Step 4. Iterate until
  `scripts/build-rt.sh rt-core` AND `rt-full` compile clean (clippy too:
  `ASCRIPT_RT=1 cargo clippy --bin ascript-rt --no-default-features --features …`).
- [ ] **Step 4: The two audited runtime sites (spec §2.3 a/b).**
  - `src/vm/run.rs` `compile_module_file` (`:1014`): under `cfg(ascript_rt)` the body
    becomes the clean Tier-2 panic
    `cannot compile module '<path>': this runtime has no compiler — the module is not
    embedded in the bundle (rebuild with the ascript toolchain)` (keep the `.aso` disk
    path in `load_file_module` untouched).
  - `src/worker/dispatch.rs`: cfg the `compile_source` arms of
    `build_code_slice_from_source` / `…for_static_method_from_source` /
    `resolve_worker_top_chunk` to the existing "the program source is unavailable"
    recoverable panic.
- [ ] **Step 5: `src/bin/ascript-rt.rs`** — no clap (spec §2.4): the worker-thread +
  current-thread-runtime bootstrap copied from `src/main.rs:475-488`; then (1) the
  embedded-shim path (reuse `try_run_embedded`'s body — extract it from `src/main.rs`
  into a shared `#[doc(hidden)] pub` helper in `src/lib.rs` so both mains call ONE
  implementation), (2) `--rt-info` JSON (version/target/tier/features from
  `cfg!(feature)`/`env!`), (3) a single path arg → `run_aso_file`, (4) usage error
  otherwise.
- [ ] **Step 6: Prove the normal world is untouched:** `cargo build`, `cargo test`,
  `cargo test --no-default-features`, `cargo clippy --all-targets`,
  `cargo clippy --no-default-features --all-targets` — ALL green;
  `cargo test --test vm_differential` both configs green; `git diff main -- src/vm
  src/interp.rs src/worker` shows ONLY `cfg(ascript_rt)` additions.
- [ ] **Step 7: Commit** — `feat(rt): ascript_rt build cfg + ascript-rt bin + runtime source-compile gates (RT §2)`.
- [ ] **Reviewer checkpoint:** reviewer builds rt-core and rt-full, runs
  `nm`/`strings` on the stub to confirm no `compile_source`/tree-sitter symbols survive,
  runs `--rt-info`, runs a `.aso` by path on the bare stub, and re-runs the full normal
  suite both configs. Reviewer also greps the diff for any non-cfg change to engine files.

## Task 2: the rt-stub battery (end-to-end on a REAL stub)

**Files:** `tests/rt_stub.rs` (new), `bench/RT_SIZE_RESULTS.md`, `.github/workflows/`
(test-job wiring), `scripts/build-rt.sh`

- [ ] **Step 1: Write the battery** — gated on `ASCRIPT_RT_BIN` (skip with a printed
  reason when unset; CI ALWAYS sets it — add a workflow step `scripts/build-rt.sh rt-full`
  + export). Reuse the `tests/native.rs` idioms (`serial_native`-style lock, `TmpDir`,
  scrubbed-PATH `run_bundle`). Cases:

```rust
// Stub bundles are built by appending with the TOOLCHAIN binary:
//   ascript build --native prog.as --stub $ASCRIPT_RT_BIN -o out      (Task 7 wires --stub;
// until then this file is committed with the flag and FAILS — it is Task 7's failing test.)
#[test] fn stub_bundle_matches_ascript_run_output() { /* hello + args forwarding */ }
#[test] fn stub_bundle_multi_module_archive_runs_from_empty_dir() { /* BNDL graph */ }
#[test] fn stub_bundle_worker_parity() { /* worker fn pool — chunk-shipping path (b) */ }
#[test] fn stub_bundle_caps_floor_and_ascript_deny() { /* --deny net at build; ASCRIPT_DENY=fs at run */ }
#[test] fn stub_missing_module_error_names_the_toolchain() { /* §2.3(a): bundle built
    WITHOUT one imported file present-on-disk-as-.as at run → exact error text */ }
#[test] fn stub_rt_info_schema() { /* parse JSON; assert version/target/tier/features */ }
#[test] fn stub_panic_diagnostics_render_from_embedded_source() { /* a panicking program:
    stderr carries the caret/source line (debug section); then build --strip → message-only */ }
```

- [ ] **Step 2:** Wire CI: the existing test workflow gains a job (or steps) building
  rt-full and running `cargo test --test rt_stub` with `ASCRIPT_RT_BIN` set. Locally
  document the same two commands at the top of the test file.
- [ ] **Step 3:** Append the per-tier size rows to `bench/RT_SIZE_RESULTS.md` (build all
  four tiers, record bytes, compute the headline "rt-X vs full toolchain" ratios). Honest
  numbers only.
- [ ] **Step 4: Commit** — `test(rt): real-stub end-to-end battery + per-tier size rows (RT §10.2, Gate 9/10)`.
- [ ] **Reviewer checkpoint:** reviewer runs the battery against a freshly built rt-core
  AND rt-full stub (the core run must skip feature-needing cases or use feature-free
  programs — verify the battery declares which tier it needs), and verifies the
  missing-module error case truly took the §2.3(a) path (not a generic import error).

## Task 3: footer flags + `--compress`

**Files:** `src/bundle.rs`, `src/lib.rs`, `src/main.rs`, `tests/native.rs`,
`tests/rt_supply_chain.rs` (new, started here)

- [ ] **Step 1: Failing tests** (in `src/bundle.rs` `mod tests` + `tests/native.rs`):
  - codec: `flags=0 ⇒ to_bytes byte-identical to pre-RT` (compare against a captured
    golden footer); `flags=FLAG_ZSTD ⇒ bundle_version=2`; reader strictness per spec
    §7.2's full matrix — v1+flags≠0 refused, v2+unknown-bit refused, version>2 refused,
    each with its message; truncation/garbage unchanged `None`.
  - end-to-end: `build --native --compress` → bundle runs byte-identically to the
    uncompressed bundle; the artifact is smaller; a compressed bundle's payload region
    starts with `uncompressed_len` and one zstd frame; `uncompressed_len` tampered
    high/low → clean refusal, no over-allocation (cap test).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `BundleFooter.reserved` → `flags` (field rename + doc;
  wire layout unchanged); `pub const FLAG_ZSTD: u16 = 1;` + `KNOWN_FLAGS`;
  `write_footer(stub_len, payload_len, aso_version, flags)` writes version
  `if flags == 0 { 1 } else { 2 }`; `validate_footer` returns a three-way
  `FooterCheck { NotABundle, Bundle { offset, len, flags }, Refused(String) }` — update
  the two call sites (`read_bundle_footer`, the shim) so `Refused` is a REPORTED error
  post-magic (never a clap fall-through; the rt bin reports identically). Payload
  encode/decode helpers: `compress_payload(bytes) -> Vec<u8>` (u64 LE len + zstd frame,
  pinned level, single-thread) and `decompress_payload(&[u8]) -> Result<Vec<u8>, String>`
  (exact-length, capped allocation) behind `cfg(feature = "bundle-zstd")` with the loud
  "built without compressed-bundle support" error otherwise. `--compress` clap flag on
  `Build` (requires `--native`); `build_native` compresses AFTER archive encode, before
  append; the shim decompresses when `FLAG_ZSTD` before the magic dispatch.
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs;
  `scripts/build-rt.sh rt-core` still clean (it includes `bundle-zstd`).
- [ ] **Step 5: Commit** — `feat(bundle): footer flags + zstd-compressed payloads — strict versioned reader (RT §7)`.
- [ ] **Reviewer checkpoint:** reviewer hand-crafts a v1 footer with nonzero flags on a
  real bundle and confirms the refusal text; flips one bit inside the zstd frame and
  confirms a clean error (no panic); measures the startup delta of a compressed hello
  bundle (record in `bench/RT_SIZE_RESULTS.md`) and confirms the UNcompressed path's
  footer bytes are bit-identical to a pre-RT build of the same program.

## Task 4: module→feature table + drift tests + the std-import scanner

**Files:** `src/rtstub/mod.rs`, `src/rtstub/std_features.rs` (new), `src/lib.rs` (module
decl, `#[cfg(not(ascript_rt))]`), `tests/rt_select.rs` (new)

- [ ] **Step 1: Failing tests:**
  - the three drift tests from spec §4.2 verbatim: STD_MODULES bijection; the
    `std_module_exports` cfg-gate parse of `src/stdlib/mod.rs` (read via
    `CARGO_MANIFEST_DIR`, a line-pair regex over `#[cfg(feature = "…")]` + `"std/…" =>`)
    equals the table; the `Cargo.toml` `[features]` parse (the non-optional `toml` crate)
    equals the checked-in closure edges and validates every named feature exists.
  - scanner: build a small multi-module archive in-test (`compile_archive` on a temp
    tree importing `std/json`, `std/fs` from a nested module, and a `./local`), assert
    `collect_std_imports(&archive) == {"std/json","std/fs"}` — read each module chunk via
    `Chunk::from_bytes_verified` and walk its `imports` table (`ImportDesc::source()`).
  - `required_features({"std/json"}) == {"data"}`;
    `required_features({"std/msgpack"}) == {"binary","data"}` (closure);
    `required_features({"std/math"}) == {}`; an unknown `std/…` specifier → Err (never
    silently feature-less — it would mean STD_MODULES drift).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `src/rtstub/std_features.rs`: the full `STD_MODULE_FEATURES`
  table (one entry per `STD_MODULES` item, `src/stdlib/mod.rs:221-279`, feature per the
  `std_module_exports` gates), `FEATURE_DEPS` (the closure edges mirrored from
  `Cargo.toml:193-294`), `collect_std_imports(&ModuleArchive) -> BTreeSet<String>`,
  `required_features(&BTreeSet<String>) -> Result<BTreeSet<&'static str>, String>`.
- [ ] **Step 4: Run — expect PASS**; clippy both configs (rtstub is
  `cfg(not(ascript_rt))` + feature-light: the table/scanner must build under
  `--no-default-features` — verify).
- [ ] **Step 5: Commit** — `feat(rtstub): module→feature table + drift gates + archive std-import scanner (RT §4.1-4.2)`.
- [ ] **Reviewer checkpoint:** reviewer mutates one cfg gate in a scratch copy of
  `stdlib/mod.rs` and confirms the drift test FAILS (then reverts); confirms the scanner
  sees an import buried in a non-entry module and a namespace import; confirms
  `std/http/server` and the other multi-segment specifiers are covered.

## Task 5: tiers, nearest-superset selection, build report + `--report-json`

**Files:** `src/rtstub/tiers.rs`, `src/rtstub/select.rs`, `src/rtstub/report.rs` (new),
`src/lib.rs`, `src/main.rs`, `tests/rt_select.rs`

- [ ] **Step 1: Failing tests:** tier chain is a strict superset chain (structural test);
  `select_tier({}) == RtCore`; `select_tier({"data"}) == RtLocal`;
  `select_tier({"net"}) == RtNet`; `select_tier({"ffi"}) == RtFull`; `--tier` downgrade
  below requirements → Err listing missing features AND the demanding modules;
  drift test: each tier's feature list, written into `scripts/build-rt.sh` (Task 1),
  matches `tiers.rs` (parse the script — one source of truth tested against the other).
  Report: `--report-json -` on a hello build emits schema-1 JSON with required/stub/
  unused features, payload + output sha256 (assert against independently computed
  hashes); the stderr report carries tier + origin + sizes.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `Tier` enum + `FEATURES` consts + `select_tier`;
  `BuildReport` struct (spec §9.2 fields) with `render_stderr()` + `to_json()`
  (serde_json, insertion-ordered keys); clap: `--tier`, `--report-json <PATH|->` on
  `Build` (require `--native`); thread through `build_native` (which gains an options
  struct — refactor its signature ONCE here: `NativeBuildOpts { target, tier, stub,
  exact, compress, oci…, report_json, no_fetch }`, defaulting to today's behavior).
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs.
- [ ] **Step 5: Commit** — `feat(rtstub): tier matrix + nearest-superset selection + build report/--report-json (RT §3/§4.4/§9.2)`.
- [ ] **Reviewer checkpoint:** reviewer builds a program importing only `std/math` and
  one importing `std/net/http`, confirms tier selection + the unused-features delta in
  both report forms, and double-builds to confirm `output_sha256` is stable.

## Task 6: signed manifest + fetch + content-addressed stub cache (fail-closed)

**Files:** `Cargo.toml` (`rt-fetch` feature + `ed25519-dalek`), `src/rtstub/manifest.rs`,
`src/rtstub/fetch.rs`, `src/rtstub/cache.rs` (new), `tests/rt_supply_chain.rs`

- [ ] **Step 1: Failing battery** (hermetic: per-test `$ASCRIPT_CACHE` tempdir under the
  `pkg::cache` env-lock discipline; manifest + stub fixtures on disk, served via an
  `ASCRIPT_RT_BASE_URL=file://…` arm in the fetcher — no network in tests):

```rust
#[test] fn happy_path_fetch_verifies_publishes_and_rehashes_on_load() {}
#[test] fn wrong_checksum_refused_nothing_published() {}
#[test] fn bad_signature_refused() {}            // signed by a different key
#[test] fn unsigned_manifest_refused() {}
#[test] fn version_mismatch_refused() {}         // manifest.ascript != CARGO_PKG_VERSION (downgrade)
#[test] fn corrupt_cache_entry_evicted_and_refetched() {}  // bit-flip the cached stub
#[test] fn truncated_stub_refused() {}
#[test] fn no_fetch_flag_skips_network_entirely() {}      // fetcher must not be called
#[test] fn cache_publish_is_atomic() {}          // stage in tmp/, rename; pre-created
                                                 // read-only slot ⇒ clean error, no partial
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `RtManifest` (spec §5.1 schema) + `verify(bytes, sig,
  pubkey)` (ed25519-dalek; the production pubkey a compiled-in const, the TEST key injected
  via a `#[doc(hidden)]` seam — never an env var); `fetch_stub(target, tier)` →
  manifest fetch (reqwest under `rt-fetch`; `file://` arm for tests) → entry lookup →
  blob fetch → sha256+size check → `cache::publish` (stage `pkg::cache::tmp_dir()`,
  chmod +x, rename to `cache_root()/rt/sha256-<hex>/ascript-rt[.exe]`);
  `cache::load(sha256)` re-hashes before returning (evict on mismatch). Without
  `rt-fetch`, the fetch rung returns a typed "fetch unavailable in this build" condition
  the ladder (Task 7) treats as availability, not integrity.
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs (incl.
  `--no-default-features`, where `rt-fetch` is off — the module must still compile its
  cache/manifest halves; gate only the network arm).
- [ ] **Step 5: Commit** — `feat(rtstub): ed25519-signed manifest + fail-closed fetch + content-addressed stub cache (RT §5.1-5.3, Gate 14)`.
- [ ] **Reviewer checkpoint:** reviewer sabotage-tests fail-closed: temporarily skip the
  signature check and confirm `bad_signature_refused` FAILS (then revert); confirms a
  refused fetch leaves `rt/` byte-identical (no partial state); audits that no code path
  trusts a cache entry without re-hashing.

## Task 7: the stub resolution ladder + un-reject `--target` + `--stub`

**Files:** `src/rtstub/select.rs`, `src/lib.rs` (`build_native`), `src/main.rs`,
`tests/native.rs`, `tests/rt_stub.rs`

- [ ] **Step 1: Failing tests:**
  - REWRITE the `--target` rejection pin (`tests/native.rs:336-356`) into the new
    contract: unknown triple → error listing the supported set; known triple with
    `--no-fetch` and no other rung → the ladder-exhausted error naming each rung's
    reason; `--target <host triple> --stub <rt bin>` builds and runs.
  - `--stub` path: bundle onto an explicit stub (this turns Task 2's battery green);
    `--stub` onto a stub that is ITSELF a bundle → overlay stripped (one payload, runs);
    `--stub` with a tier-insufficient stub (rt-core stub, program imports `std/json`) →
    fail-closed feature error via `--rt-info` probe.
  - platform-independence: build the same program `--stub A` and `--stub B` (two
    different stub binaries) → the `payload || footer-minus-offset` bytes are identical
    (extract via the footer; compare).
  - current_exe fallback: with no `--stub`, `--no-fetch`, and no sibling → bundle built
    from `current_exe()` + EXACTLY ONE stderr warning line; bundle runs (today's
    behavior preserved).
  - sibling rung: place a built `ascript-rt` beside the toolchain binary (copy into a
    tempdir with the toolchain) → it is chosen, `--rt-info` validated.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `resolve_stub(opts) -> Resolved { bytes_path, origin,
  sha256, features: Option<Vec<String>> }` walking spec §5.4's five rungs with the
  integrity-vs-availability split; overlay-strip reuses `read_bundle_footer` exactly as
  `build_native` does for itself (`src/lib.rs:1565-1568`); `build_native` replaces the
  `:1510-1517` rejection with triple validation + the ladder; the macOS sign call
  (`:1627`) now runs ONLY for stubs produced locally on a mac host (current_exe rung —
  unchanged; `--exact` comes in Task 8); fetched/`--stub` stubs are appended to AS-IS
  (pre-signed, spec §6.2 — add the explanatory comment citing the BIN rule at
  `src/lib.rs:1586-1592`). Windows-target naming (`.exe` by target not host).
- [ ] **Step 4: Run — expect PASS**; the FULL `tests/native.rs` + `tests/rt_stub.rs`
  (with `ASCRIPT_RT_BIN`) green; suite + clippy both configs.
- [ ] **Step 5: Commit** — `feat(rt): stub resolution ladder + --stub + --target cross-append (RT §5.4/§6)`.
- [ ] **Reviewer checkpoint:** reviewer verifies on macOS (where available) that a bundle
  appended to a locally-built-and-signed rt stub passes `codesign --verify` on the stub
  region and EXECUTES (the arm64 SIGKILL smoke); verifies the warning prints once and
  names the real reasons; probes `--target` with a garbage triple and a valid-but-
  unfetched one.

## Task 8: `--exact`

**Files:** `src/rtstub/exact.rs` (new), `src/main.rs`, `tests/rt_select.rs` (unit),
`tests/rt_stub.rs` (one end-to-end, CI-gated)

- [ ] **Step 1: Failing tests:** command construction is PURE and unit-tested:
  `exact_build_plan(required, target)` returns the exact
  `["build","--release","--bin","ascript-rt","--no-default-features","--features",
  "<sorted,set,bundle-zstd>", …]` argv + env (`ASCRIPT_RT=1`, `ASCRIPT_RT_TIER=custom`);
  missing cargo → the specific error; `$ASCRIPT_SRC` unset / version-mismatched
  (fixture Cargo.toml) → each specific error; `--exact --target aarch64-apple-darwin` on
  a non-mac host → the rejection (spec §4.5). One CI-gated end-to-end (env
  `ASCRIPT_SRC=$PWD` in the repo): `build --native --exact` a `std/math`-only program →
  stub built, content-addressed into the cache, bundle runs; second build reuses the
  cache (assert no second cargo invocation via a probe seam).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement:** detection (cargo on PATH, `$ASCRIPT_SRC` + version check),
  invocation (inherit stderr so cargo's own errors surface verbatim), post-build: macOS
  host+target → `adhoc_sign_macos` BEFORE caching (the BIN rule); sha256 → publish via
  Task 6's cache + an `exact-index` sidecar mapping `(version,target,features-hash) →
  sha256` for reuse; wire `--exact` into the ladder as a rung-0 override (mutually
  exclusive with `--tier`/`--stub` — clap `conflicts_with`).
- [ ] **Step 4: Run — expect PASS** (CI job exports `ASCRIPT_SRC`); suite + clippy both
  configs.
- [ ] **Step 5: Commit** — `feat(rt): --exact local-cargo stubs — detect, build, sign, content-address (RT §4.5)`.
- [ ] **Reviewer checkpoint:** reviewer runs the end-to-end locally, times the cached
  second build, confirms the feature set in the built stub's `--rt-info` equals the
  program's requirements exactly (no tier slack), and confirms the non-mac darwin
  rejection text.

## Task 9: `--oci`

**Files:** `src/rtstub/oci.rs` (new), `src/main.rs`, `tests/rt_oci.rs` (new)

- [ ] **Step 1: Failing structural tests** (hermetic, no docker): build
  `--oci app.as -o app.tar` (host-arch musl target; under test use `--stub` with ANY
  executable bytes — the oci writer is agnostic) and assert, by unpacking in-process:
  `oci-layout` content; `index.json` schemaVersion/mediaType/platform/ref.name
  annotation (default `<stem>:latest`, `--oci-tag` override); manifest descriptor
  digests+sizes all verify against blob bytes; config `architecture`/`os`/`Entrypoint`/
  `diff_ids`; `diff_id == sha256(uncompressed inner tar) != layer blob digest ==
  sha256(gzip bytes)`; inner tar = exactly `/app`, mode 0755, uid/gid 0, mtime 0, bytes ==
  the bundle; double-build → byte-identical tar; `SOURCE_DATE_EPOCH=1700000000` →
  timestamps follow, still deterministic. Rejections: `--oci --target *-gnu` → musl
  error; `--oci` on a windows/darwin triple → error; `--oci` without an available musl
  stub rung → ladder error.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** the writer (the `tar` + `flate2` crates ride the existing
  optional deps — gate `oci` support under `compress`-availability or add direct
  always-on? DECIDE: gate `--oci` on `cfg(feature = "compress")` with a clean
  "rebuild with compress" error otherwise — the toolchain default has it; record the
  decision in the spec status header), per spec §8: deterministic inner tar → gz →
  digests → config/manifest/index JSON (insertion-ordered compact serde_json) → outer
  tar (sorted: `oci-layout`, `index.json`, `blobs/...`). Target defaulting
  (`<host-arch>-unknown-linux-musl`) + arch mapping + rejections.
- [ ] **Step 4: The docker integration test:** `#[test] fn docker_load_and_run()` —
  probe `docker version` (else print-skip); `docker load -i app.tar`, `docker run --rm
  <tag>` of a hello program (built on a REAL musl rt stub when `ASCRIPT_RT_BIN_MUSL` is
  provided by CI; else skipped with reason), assert stdout; `docker rmi` cleanup.
- [ ] **Step 5: Run — expect PASS** (structural always; docker where present); suite +
  clippy both configs.
- [ ] **Step 6: Commit** — `feat(rt): --oci loadable OCI image tarball — deterministic, dockerless (RT §8)`.
- [ ] **Reviewer checkpoint:** reviewer validates `app.tar` with an independent tool
  where available (`skopeo inspect oci-archive:app.tar` or docker), re-checks every
  digest by hand once, and confirms reproducibility across two machines-worth of env
  perturbation (different cwd, different TZ, different umask).

## Task 10: reproducibility battery + report schema lock

**Files:** `tests/rt_stub.rs` / `tests/rt_oci.rs` (extend), `tests/rt_select.rs`

- [ ] **Step 1:** The cross-flag double-build battery: for each of {plain, `--compress`,
  `--target` (via `--stub`), `--oci`, `--compress --oci`} build twice → bit-identical
  outputs AND identical `--report-json` (modulo nothing — the report contains no
  timestamps by design; assert that). Schema lock: a golden `report.schema.json`-style
  assertion (field presence + types) so CI consumers get a versioned contract; bumping
  requires `"schema": 2`.
- [ ] **Step 2: Run — expect PASS** (failures here are real determinism bugs — fix them
  in-branch, never loosen to "almost equal").
- [ ] **Step 3: Commit** — `test(rt): reproducibility battery + report-json schema lock (RT §9, Gate 14)`.
- [ ] **Reviewer checkpoint:** reviewer injects a deliberate `SystemTime::now()` into the
  report in a scratch build and confirms the battery catches it (then reverts).

## Task 11: release infrastructure — the stub matrix, manifest generation, signing

**Files:** `scripts/release-rt-stubs.sh` (new), `.github/workflows/release-rt.yml` (new),
`src/rtstub/manifest.rs` (generator half), `tests/rt_supply_chain.rs`

- [ ] **Step 1: Failing test:** the manifest GENERATOR is in-tree and hermetically
  tested: `generate_manifest(version, entries) -> (json_bytes, …)` + `sign` with a test
  key → `verify` round-trips; entry filenames follow the spec §5.1 convention; the
  generated JSON is canonical (double-generate → identical bytes).
- [ ] **Step 2:** `scripts/release-rt-stubs.sh`: for each target×tier (spec §3.3 — 8×4),
  `scripts/build-rt.sh <tier> --target <triple>`; darwin stubs ad-hoc signed post-build
  (runs on the mac runner); compute sha256s; invoke the generator
  (`cargo run -- …` hidden subcommand `ascript rt-manifest-gen` OR a `#[doc(hidden)]`
  bin — pick the hidden-subcommand route, it tests better); sign with the release key
  from CI secrets.
- [ ] **Step 3:** `.github/workflows/release-rt.yml`: a tag-triggered matrix
  (ubuntu/macos/windows runners; musl targets via `rustup target add` + musl-cross on
  the ubuntu runner) uploading stubs + `rt-manifest.json` + signature to the GitHub
  release. **The musl feasibility spike lives here:** if `rusqlite`/rustls fail under a
  musl target, fix (vendored toolchain config) or NARROW the published matrix with an
  owner note in the spec status header + `roadmap.md` — a recorded decision, never a
  silent green-but-absent artifact.
- [ ] **Step 4:** Key handling: generate the production keypair OUT of band; commit the
  PUBLIC key const; document custody/rotation (a new key ⇒ a toolchain release) in
  `CONTRIBUTING.md`'s release runbook section.
- [ ] **Step 5: Run** the generator tests; dry-run the script locally for the host
  target (`scripts/release-rt-stubs.sh --host-only --key <testkey>`); attach output to
  the task log.
- [ ] **Step 6: Commit** — `ci(rt): release stub matrix + signed manifest generation (RT §3.3/§5.1)`.
- [ ] **Reviewer checkpoint:** reviewer runs the host-only dry run, verifies the produced
  manifest against Task 6's verifier with the test key, and confirms the workflow's
  secret usage never echoes the private key (audit the yml).

## Task 12: docs, status updates, holistic review, FINAL GATES, merge

**Files:** `docs/content/cli.md`, `docs/content/language/bundles.md`, `CLAUDE.md`,
`goal-perf.md`, `superpowers/roadmap.md`,
`superpowers/specs/2026-06-12-native-runtime-stubs-design.md` (status header)

- [ ] **Step 1: Docs.**
  - `docs/content/cli.md` `build` section: `--target` (supported triples), `--tier`,
    `--exact` (+ `ASCRIPT_SRC`), `--stub`, `--no-fetch`, `--compress`, `--oci`/
    `--oci-tag`, `--report-json`; the stub ladder + the one-time current_exe warning;
    `ASCRIPT_RT_BASE_URL`, `ASCRIPT_RT_NO_FETCH`, `$ASCRIPT_CACHE/rt`.
  - `docs/content/language/bundles.md`: a "Runtime stubs" section (what a stub is, the
    tier table with MEASURED sizes from `bench/RT_SIZE_RESULTS.md`, import-driven
    selection + how to read the build report, fail-closed fetching), a "Cross builds"
    section (platform-independent payload; the macOS pre-signed-stub explanation citing
    the sign-before-append rule), "--compress", and "Container images (--oci)"
    (docker/podman load, musl/scratch, `ASCRIPT_DENY` via `-e`).
  - Confirm NO new NAV entry is needed (both are existing pages — `language/bundles` is
    `docs/assets/app.js:27`); record the check (Gate 13). Update the README CLI table
    row for `build`.
- [ ] **Step 2: Status.** `CLAUDE.md`: an RT paragraph (the `ascript_rt` cfg + why not a
  feature; the two gated runtime source-compile sites; tiers; fail-closed
  manifest/cache; footer flags; `--oci`; "stubs are version-locked to the toolchain").
  `goal-perf.md`: RT 🏗️ → ✅ in the track table with the measured headline (full vs
  rt-core/rt-full sizes). `superpowers/roadmap.md`: the RT milestone + recorded futures
  (SBOM, interp-eval carve-out if Phase-0-material, `--push`, WASM stubs pointer) — each
  owner-noted. Spec status header → `Implemented (merged <sha>)` + any deltas-from-spec.
- [ ] **Step 3: FINAL GATES CHECKLIST** (every box requires pasted command output —
  evidence before assertions):
  - [ ] `cargo clippy --all-targets` AND `cargo clippy --no-default-features
        --all-targets` clean; `ASCRIPT_RT=1 cargo clippy --bin ascript-rt
        --no-default-features --features <rt-full set>` clean.
  - [ ] `cargo test` AND `cargo test --no-default-features` green.
  - [ ] `cargo test --test vm_differential` green in BOTH configs; `git diff main --
        src/vm src/interp.rs src/worker` contains ONLY `cfg(ascript_rt)`-gated additions
        (justified line-by-line).
  - [ ] `tests/native.rs` fully green (the `--target` pin rewritten, nothing deleted);
        `tests/{rt_stub,rt_supply_chain,rt_oci,rt_select}.rs` green with
        `ASCRIPT_RT_BIN` set; the docker test green where docker exists (reason-printed
        skip otherwise).
  - [ ] Supply-chain battery green INCLUDING the reviewer sabotage re-checks (signature
        skip → test fails; stale-cache trust → test fails).
  - [ ] `ASO_FORMAT_VERSION` still 27, `ARCHIVE_VERSION` still 1 (`git diff main --
        src/vm/aso.rs src/vm/archive.rs` shows no constant change);
        `BUNDLE_FOOTER_VERSION` semantics per spec §7.2 with the Task-0 pins green.
  - [ ] `cargo test --release --test vm_bench -- --ignored --nocapture`: spec/tw
        geomean ≥2× holds (Gate 17 — RT touches no engine path; prove it anyway).
  - [ ] `bench/RT_SIZE_RESULTS.md` complete: full baseline, per-feature, per-tier,
        compressed-startup delta; every doc size claim traces to it.
  - [ ] Docs build/serve sanity (`cd docs && python3 -m http.server` spot-check), NAV
        no-change check recorded; README updated.
  - [ ] No new `unwrap`/`expect`/`panic!` reachable from untrusted input (footer bytes,
        manifest, fetched/cached stubs, compressed payloads, OCI inputs) — reviewer grep
        + justification list.
- [ ] **Step 4: Holistic review** — a fresh reviewer subagent reviews the WHOLE branch
  against the spec: the §2.3 audit table vs the actual cfg sites (hunt a third
  `compile_source` path); §5.4's integrity/availability split vs the real control flow;
  §7.2's matrix vs the reader code; cross-task interactions (`--exact` + `--compress` +
  `--oci` composed; a `--stub` that is a bundle; the WARM compile-cache never caching RT
  artifacts); and latent bugs in neighbors (`build_native` post-refactor, the shim
  extraction, `try_run_embedded` error paths). All findings fixed in-branch with
  regression tests before merge.
- [ ] **Step 5: Merge** — `git checkout main && git merge --no-ff
  feat/native-runtime-stubs` with a summary message (house trailer). Update the
  `goal-perf.md` status table post-merge.

---

## Standing rules for every task (repeated so no subagent misses them)

1. **Bug-fix discipline:** any defect encountered — RT code or pre-existing — gets a
   failing-test-first fix in this branch, logged in the task notes with its commit.
2. **Never weaken fail-closed.** An integrity failure (signature, checksum, version,
   cache re-hash) aborts; only availability falls through the ladder. A test that wants
   to "just make it pass" by skipping verification is a bug.
3. **Both feature configs + the stub build, every time.** A task is not green until
   `--no-default-features` is AND `scripts/build-rt.sh rt-core` compiles clean.
4. **Engine files take cfg-gated additions ONLY.** Any behavioral diff to
   `src/vm`/`src/interp.rs`/`src/worker` outside `cfg(ascript_rt)` is a Gate-1 incident.
5. **Honest numbers only:** every size/startup claim in docs/reports traces to a
   committed measurement in `bench/RT_SIZE_RESULTS.md`. Expectations stated; results
   measured; shortfalls reported, not massaged.
6. **No env reads in parallel tests;** thread keys/paths/flags through `#[doc(hidden)]`
   seams; `$ASCRIPT_CACHE` tests take the `pkg::cache` locks.
