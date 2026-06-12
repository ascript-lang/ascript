# AScript Runtime-Only Native Stubs — Tier Matrix, Import-Driven Pruning, Cross Builds, OCI & Compression — Design (RT)

- **Status:** Proposed (no branch yet — design only)
- **Date:** 2026-06-12
- **Code:** RT (the foundation of goal-perf.md's "Deployment & reach track"; CNTR's images
  build on it)
- **Depends on:** shipped BIN (`src/bundle.rs` footer codec + `build_native`,
  `src/lib.rs:1504`; the pre-clap shim `try_run_embedded`, `src/main.rs:558`), shipped BNDL
  (`ASCRIPTA` module archive + tree-shaker `src/compile/shake.rs` + capability embedding +
  `run_verified_archive`, `src/lib.rs:1808`), the SP6 content-addressed cache
  (`src/pkg/cache.rs`, `src/pkg/hash.rs`), the FUZZ-hardened `.aso`/archive readers.
- **Engines:** **none touched.** RT is CLI-side + link-level (a second bin target and a
  build-time cfg). The VM, tree-walker, `.aso` layout, and archive layout are unchanged:
  `ASO_FORMAT_VERSION` stays **27** (`src/vm/aso.rs:167`), `ARCHIVE_VERSION` stays **1**
  (`src/vm/archive.rs:27`). `tests/vm_differential.rs` must be structurally unaffected
  (§10.4). The ONE wire change is to the **bundle footer** (`BUNDLE_FOOTER_VERSION` 1→2
  *only for compressed payloads*, §7), which versions the outermost container only.
- **Breaking:** no. `ascript build --native app.as` with no new flags resolves a stub
  through a ladder whose last rung is today's exact behavior (`current_exe()` as the stub,
  now with a one-time stderr warning, §5.4). All existing bundles keep running; all
  existing `tests/native.rs` tests are preserved.
- **Owner decision baked in:** **v2 upfront.** There is no minimal-v1 staging — the bin
  target, the tier matrix, fetch/verify/cache, import-driven selection, `--exact`,
  `--target`, `--oci`, `--compress`, and reproducible outputs all ship in this one spec.

---

## 0. Read this first — what RT is and is not

`ascript build --native` today appends the verified payload to a copy of **the running
toolchain binary** (`current_exe()`, `src/lib.rs:1561-1568`) — the full ~42 MB `ascript`
carrying the LSP, DAP, formatter, REPL, package manager, static checker, three parsers
(legacy, CST, tree-sitter), and the doc generator. None of that is reachable from a bundle:
the shim runs the embedded payload **before clap ever parses argv**
(`src/main.rs:597-602`), so every toolchain subsystem in a bundle is dead weight shipped to
every user of every bundled program.

RT ships **`ascript-rt`** — a runtime-only bin target (VM + GC + stdlib + workers + caps +
`.aso`/archive loader+verifier + panic diagnostics + the embedded-payload shim) — and a
**prebuilt, per-target tier matrix** of it, so `build --native` appends the payload to a
small stub matched to what the program actually imports. This is still **bundling, not
AOT**: the stub interprets the same verified bytecode. The payload is platform-independent
(§6.1), which is what makes `--target` a one-line append instead of a cross-compiler.

What RT is **not**: not a JIT/AOT story (JIT spec), not WASM (separate spec, parked), not
self-update, not an SBOM generator (§11).

## 1. Current state (verified)

- **Stub = `current_exe()`.** `build_native` (`src/lib.rs:1504`) copies the running binary,
  strips a pre-existing overlay if the builder is itself a bundle (`:1565-1568`), writes to
  a temp sibling, chmods, **ad-hoc signs on macOS BEFORE appending** (`:1586-1592`, the
  CRITICAL ordering comment; signer `bundle::adhoc_sign_macos`, `src/bundle.rs:147-155`),
  reads the signed stub's on-disk length back (signing rewrites `__LINKEDIT`,
  `src/lib.rs:1629-1634`), appends `payload || footer`, and atomically renames
  (`:1651-1662`).
- **Footer** (`src/bundle.rs`): fixed 32 bytes — `payload_offset:u64 · payload_len:u64 ·
  aso_version:u32 · bundle_version:u16 · reserved:u16 · magic:"ASCRIPTB"`
  (`FOOTER_SIZE`, `:31`). `reserved` is documented "future flags without a layout bump"
  (`:46`). **`BundleFooter::from_bytes` checks ONLY length + magic — it ignores
  `bundle_version` and `reserved`** (`:66-82`); `validate_footer` adds the
  `MIN_STUB_SIZE`/overflow/region bounds (`:124-141`). This tolerance is load-bearing for
  §7's strictness design and is pinned as a baseline test before being changed.
- **Shim**: `try_run_embedded` (`src/main.rs:558-595`) — O(1) tail read, validate, slice,
  `run_embedded_aso` (`src/lib.rs:1696`) → magic-dispatch to the bare-chunk or `ASCRIPTA`
  archive runner (`src/lib.rs:1757-1761`). Post-magic-confirmation read failures are
  REPORTED errors, never a fall-through to clap (`src/main.rs:574-585`).
- **`--target` is parsed-but-rejected**: clap arg at `src/main.rs:84-85`
  (`requires = "native"`), rejection with a specific Tier-1 error at
  `src/lib.rs:1510-1517`, pinned by `tests/native.rs:336-356`. **This spec un-rejects it.**
- **Caps ride the archive manifest** (BNDL): composed by the same `compose_caps`
  (`src/main.rs:311`) the run path uses, embedded at `src/lib.rs:1540`, enforced at run +
  monotone `ASCRIPT_DENY` subtraction (`src/lib.rs:1711-1736`).
- **Tree-shaker**: `src/compile/shake.rs` — module graph (`ModuleNode`/`ImportEdge`,
  `:59-91`), per-module keep-sets, deterministic `ShakeReport::digest` (`:153`). Its
  import-graph walk is RT's feature-selection input (§4.1).
- **Content-addressed store**: `src/pkg/cache.rs` (`cache_root()` honoring
  `$ASCRIPT_CACHE`, `:26`; `store/<asum1-…>` `:89`; `tmp/` staging `:100`) and
  `src/pkg/hash.rs` (`asum1_tree`, `:30`) — the atomic-publish/verify hygiene RT's stub
  cache reuses (§5.3).
- **The feature graph**: `Cargo.toml:168` (`default`) + the per-feature definitions
  (§3.1); `src/stdlib/mod.rs:114-211` is the module→`cfg(feature)` truth RT's table is
  drift-tested against (§4.2); `STD_MODULES` (`:221-279`) is the feature-independent
  module census.

## 2. The `ascript-rt` bin target

### 2.1 What is in, what is out

**In (the runtime):** the VM (`src/vm/`), GC, the `Interp` runtime kernel (stdlib dispatch,
native resources, caps gate, determinism seams), the full feature-selected stdlib, the
worker machinery (`src/worker/` — core/unconditional), `std/caps` (core), the `.aso` +
archive loaders/verifiers (`aso.rs`, `archive.rs`, `verify.rs`), panic diagnostics
(ariadne over the **embedded** debug-section source, §2.3e), the bundle footer codec +
embedded-payload shim, `ASCRIPT_DENY`, and the worker chunk-shipping path.

**Out (the toolchain):** LSP, DAP, `ascript doc`, the formatter, the REPL, the package
manager, the static checker, the legacy parser + lexer entry points, the CST front-end
(`src/syntax/`), the bytecode compiler (`src/compile/`), tree-sitter (the vendored
`parser.c` is not even compiled, §2.4), clap (the rt main parses its own three-case argv),
and rustyline/cstree/tower-lsp by consequence.

### 2.2 The gating mechanism — a build-time cfg, NOT a cargo feature

The frontend cannot be gated by a Cargo feature: features are additive, and the test matrix
contract (`cargo test` AND `cargo test --no-default-features`, goal.md Gate 3) requires the
parsers/compiler to build with **no** features enabled. A `frontend` default feature +
`required-features = ["frontend"]` on the `ascript` bin would stop the bin building under
`--no-default-features`, breaking every `env!("CARGO_BIN_EXE_ascript")` integration test
(`tests/native.rs:16-18` et al.). Rejected (§11).

Instead RT uses the **`fuzzing`-cfg precedent** (`Cargo.toml:18-19`): `build.rs` reads an
env var and emits a cfg.

```rust
// build.rs (addition)
println!("cargo:rerun-if-env-changed=ASCRIPT_RT");
if std::env::var_os("ASCRIPT_RT").is_some_and(|v| v == "1") {
    println!("cargo:rustc-cfg=ascript_rt");
    // …and SKIP the tree-sitter `cc` compile of tree-sitter-ascript/src/parser.c —
    // the C parser is never linked into a stub.
}
```

`cfg(ascript_rt)` is registered in `[lints.rust] check-cfg` beside `cfg(fuzzing)`. A stub
build is:

```
ASCRIPT_RT=1 cargo build --release --bin ascript-rt \
    --no-default-features --features <tier feature set>
```

Gated `#[cfg(not(ascript_rt))]`: `src/syntax/`, `src/compile/`, `src/parser.rs`,
`src/lexer.rs`'s parser-facing entry points, `src/check/`, `src/fmt.rs`, `src/repl.rs`,
the toolchain entry points in `src/lib.rs` (`run_file`/`run_source`/`run_tests`/
`build_file`/`build_native`/`compile_archive`), and `src/main.rs`'s clap CLI (under the
cfg, `src/main.rs` compiles to a tiny "this is a toolchain build misconfiguration" stub
main so `cargo build --bins` under the cfg still fails loudly rather than confusingly).
The feature-gated toolchain subsystems (`lsp`/`dap`/`doc`/`pkg`/`profile`) are simply not
enabled in tier builds — no new gating needed.

**Normal builds are byte-identical:** without `ASCRIPT_RT=1` the cfg is never set, so
`cargo build`, `cargo test`, `cargo test --no-default-features`, clippy in both configs,
and `vm_differential` compile exactly today's code (§10.4).

### 2.3 The residual source-parsing audit (the PROOF the runtime needs no parser)

Archives embed pre-compiled chunks (BNDL) and worker code-shipping is chunk-based over
`bcanalysis` — but "mostly chunk-based" is not a proof. Every runtime path that can reach
`compile_source`/a parser was enumerated and resolved:

| # | Path | Where (verified) | Reachable from a stub? | Resolution |
|---|---|---|---|---|
| a | **Runtime module compile on archive miss.** `load_file_module`'s disk fallback compiles a sibling `.as` at runtime. | `Vm::compile_module_file`, `src/vm/run.rs:1014-1047`, called from `load_file_module` (`:741`) | Yes — a bundle whose import misses the archive and finds a `.as` on disk | Under `cfg(ascript_rt)` the `.as` arm raises a clean Tier-2 panic: `cannot compile module '<path>': this runtime has no compiler — the module is not embedded in the bundle (rebuild with the ascript toolchain)`. The `.aso` disk fallback stays (the verifier is in the runtime). Honest narrowing, loud, tested. |
| b | **Worker code-shipping, source mode.** When a program runs FROM SOURCE, slices recompile the retained source. | `build_code_slice_from_source` (`src/worker/dispatch.rs:829-844`), the static-method variant (`:849-863`), and `resolve_worker_top_chunk`'s source-preferred arm (`:874-894`) | No — `worker_source` is set only by the gated-out source entry points (`src/lib.rs:311,647,853,…`); bundles set `worker_aso_bytes` (`src/lib.rs:1778`) and `worker_archive_bytes` (`:1863`), the chunk path | Under the cfg, the `compile_source` arms collapse to the existing "the program source is unavailable" recoverable panic (already the no-source fallback message). The chunk path is proven by the existing `native_worker_bundle_parity` test and re-proven on a real stub (§10.2). |
| c | **DAP `evaluate`** re-parses expressions on the tree-walker over the paused frame. | `src/dap/` (feature `dap`, `Cargo.toml:253`) | No — `dap` is excluded from every tier | Feature absent; nothing to gate. |
| d | **REPL** incremental parsing. | `src/repl.rs` | No | `#[cfg(not(ascript_rt))]`. |
| e | **Panic diagnostics need source text.** ariadne renders carets from source. | The DBG debug section embeds each module's source into the `.aso` (`SourceInfo` bound via `chunk.set_module_source`, `src/vm/run.rs:1038-1045`); `build --strip` removes it (`src/main.rs:73-76`) | Yes — and it works WITHOUT a parser: the source is data, not something to parse | ariadne stays in the runtime. A `--strip`ped artifact degrades to span-less messages — already-shipped behavior, unchanged. |
| f | **Static checker on run?** | `ascript run` never runs `check` (separate subcommand, `src/main.rs:100-129`) | No | `#[cfg(not(ascript_rt))]` on `src/check/`. |
| g | **The tree-walker engine** evaluates legacy AST produced only by parsing. | `src/interp.rs` eval; engine selection ignored for `.aso` (CLAUDE.md) | No — every entry that parses is gated out | The `Interp` KERNEL (state, stdlib `call`, resources, caps, workers, det) stays — it is the runtime. The eval functions become statically unreachable and are removed by the linker; carving them out of `interp.rs` textually is an evidence-gated follow-up ONLY if Phase 0 (§2.5) measures them material after dead-stripping. Recorded, not silent. |
| h | **Data parsers** (`json.parse`, regex, toml/yaml/csv) | stdlib | Yes | Parse DATA, not AScript source — in scope by design. |

Conclusion: exactly **two** production code sites ((a), (b)) can reach the compiler at
runtime; both get loud, tested, cfg-gated refusals. Everything else is gated at the module
or feature level. The proof is enforced by a tripwire test: a stub build must contain **no
`compile_source` symbol** (Task 1 asserts the gated sites compile under the cfg, and the
rt battery (§10.2) runs a bundle whose import would have taken path (a) and asserts the
exact error).

### 2.4 The `ascript-rt` bin surface

`src/bin/ascript-rt.rs` (always compiles; only size-optimal under the cfg) — no clap:

1. **Bundled** (footer present): the existing shim path — run the payload, forward all
   argv. Identical semantics to today's bundled `ascript`.
2. **Bare stub + `--rt-info`**: print one JSON line — `{"name":"ascript-rt",
   "version":CARGO_PKG_VERSION,"target":TARGET,"tier":env!("ASCRIPT_RT_TIER"),
   "features":[…cfg!(feature)-derived…],"aso_format_version":27,
   "archive_version":1}` — the builder's introspection hook (§5.4 rung 3) and the
   release-manifest generator's input. `ASCRIPT_RT_TIER` is stamped at stub-build time.
3. **Bare stub + a path argument**: run it as a verified `.aso`/`ASCRIPTA` artifact
   (`run_aso_file` semantics) — the container/dev convenience (an OCI layer can mount a
   payload beside a stub) and the test seam. Anything else → a two-line usage error.

### 2.5 Phase 0 — measurement mandate (no size claim without a number)

Plan Task 0 builds the size matrix BEFORE any cut is designed in stone:
`cargo bloat --release` (or `--crate-type` section analysis where bloat is unavailable) on
(a) the full default `ascript`, (b) `ascript-rt` per tier, (c) per-feature deltas
(each runtime feature toggled on rt-core), recorded in `bench/RT_SIZE_RESULTS.md` with the
toolchain-vs-runtime split (how many MB the gated front-end + tree-sitter + clap +
rustyline + cstree + tower-lsp actually cost). **Every size number in docs/README/report
output traces to that table.** Expectations may be stated ("a stub should be a fraction of
42 MB"); results are measured — the goal-perf Gate-16 discipline applied to bytes instead
of nanoseconds.

## 3. The stub tier matrix

### 3.1 Feature census (from the real graph, `Cargo.toml`)

**Toolchain-only — excluded from every tier:** `lsp` (:235), `dap` (:253), `doc` (:243),
`pkg` (:193 — bundles embed package modules under the `pkg/<specifier>` archive namespace,
BNDL §3.3; the runtime never fetches), `profile` (:266 — `run --profile` is CLI surface a
bundle's argv-forwarding shim never exposes), `fuzzgen` (:172, test-only), `http3` (:228,
opt-in + `RUSTFLAGS`-unstable; a program pinning `httpVersion:"3"` on a stub gets the
existing clean Tier-1 error — documented).

**Runtime features (19):** `shared`, `data`, `binary`, `log`, `workflow`, `datetime`,
`crypto`, `compress`, `sys`, `sysinfo`, `sql`, `tui`, `net`, `postgres`, `redis`,
`telemetry`, `intl`, `ai`, `ffi`.

**New tiny feature:** `bundle-zstd = ["dep:zstd"]`, added to `default` AND to every tier
build — the stub-side payload decompressor (§7). Kept separate from `compress` so rt-core
can decompress its own payload without shipping `std/compress`.

### 3.2 The tiers (a curated superset CHAIN — four, justified by the dependency cliffs)

The cut lines follow the three real weight cliffs in the graph: (1) the serde/data layer,
(2) the network stack (`reqwest`+`hyper`+rustls, `Cargo.toml:223`), (3) the three heavies
(`icu` compiled_data :209, `genai` :285, `libloading`+`libffi` :294).

| Tier | Feature set (cumulative) | Who it serves |
|---|---|---|
| **rt-core** | `shared`, `bundle-zstd` | Pure-compute CLI tools: the full core language, workers, caps, and every unconditional std module (math/string/array/object/map/set/lru/events/template/bytes/caps/convert/task/time/sync/stream/assert/bench/cli/color/decimal/schema). |
| **rt-local** | + `data`, `binary`, `log`, `workflow`, `datetime`, `crypto`, `compress`, `sys`, `sysinfo`, `sql`, `tui` | Local-machine batteries — files, processes, env, sqlite, terminal, json/msgpack/dates/crypto — with **no network stack and none of the heavies**. |
| **rt-net** | + `net`, `postgres`, `redis`, `telemetry` | The server tier: HTTP(S)/WS/TCP/UDP clients + servers, DB clients, telemetry exporters. |
| **rt-full** | + `intl`, `ai`, `ffi` | Everything runtime-shaped — the default-feature set minus the toolchain-only features. |

A chain makes nearest-superset selection (§4.4) total and unambiguous. Per-program exact
prebuilds are rejected (§11) — 2^19 combinations is what the chain + `--exact` exist to
avoid.

### 3.3 Targets (v2 published set)

`{x86_64, aarch64} × {apple-darwin, unknown-linux-gnu, unknown-linux-musl,
pc-windows-msvc}` — 8 triples × 4 tiers = 32 release artifacts. musl stubs are
statically linked (the `--oci` requirement, §8.3). Anything else: no prebuilt → the ladder
(§5.4) and `--exact` (§4.5) are the answers, with a clear error naming the supported set.

## 4. Import-driven selection

### 4.1 Input: the archive's own chunks (not the source)

After `compile_archive` produces the (shaken) archive, the builder scans **every embedded
module chunk's `imports` side table** (`ImportDesc::source()`, `src/vm/chunk.rs:195-224` —
the exact table `Op::Import` indexes, and the same facts the shaker's edge-builder consumes
at `src/lib.rs:1337-1356`) and collects every specifier starting `std/`. This is
chunk-level truth: it cannot drift from what the runtime will execute, it sees imports the
shaker kept anywhere in the graph (import statements are side-effecting and always kept,
shake.rs module doc), and it covers worker code slices by construction (slices derive from
the same chunks).

`required_features = closure(⋃ table[module] for module in std_imports)` where `closure`
applies the Cargo feature-dependency edges (`binary→data`, `log→data`, `workflow→data`,
`pkg→net+compress`, … — `Cargo.toml:193-294`).

### 4.2 The checked-in module→feature table + drift tests (the tests-as-gates pattern)

A new CLI-side module `src/rtstub/std_features.rs` (compiled `#[cfg(not(ascript_rt))]`;
the data is toolchain-side):

```rust
/// (canonical std specifier, required Cargo feature or None for core).
/// DRIFT-TESTED against src/stdlib/mod.rs's actual cfg gates AND against
/// stdlib::STD_MODULES — do not edit one without the others.
pub const STD_MODULE_FEATURES: &[(&str, Option<&str>)] = &[
    ("std/math", None), …, ("std/json", Some("data")), ("std/ffi", Some("ffi")), …
];
```

Three drift tests (all hermetic, reading the repo via `CARGO_MANIFEST_DIR` — the FFI
completeness-test precedent):

1. **Completeness:** every entry of `stdlib::STD_MODULES` (`src/stdlib/mod.rs:221`) appears
   exactly once in the table, and vice versa.
2. **Gate drift:** parse `src/stdlib/mod.rs`'s `std_module_exports` match
   (`#[cfg(feature = "X")]` + `"std/Y" =>` pairs, `:114-211`) and assert the extracted
   `(module → feature)` mapping equals the table. A new/regated module fails this test
   until the table is updated.
3. **Closure drift:** parse `Cargo.toml`'s `[features]` (the `toml` crate is a
   non-optional dep) and assert the checked-in feature-dependency closure used by §4.1
   matches the manifest's actual edges, and that every feature the table names exists.

### 4.3 Tier validation is structural, not advisory

The builder errors (fail-closed) if the chosen stub's feature set is not a superset of
`required_features` — whether the stub came from the matrix (features known from the signed
manifest, §5.1), a dev sibling (features read via `--rt-info`, §2.4), or `--stub`
(`--rt-info` probed when the stub is executable on this host; for a cross-target `--stub`
the user asserts compatibility and the report records `features: unverified`). The runtime
backstop is unchanged: a missing module is today's clean unknown-module error.

### 4.4 Selection + overrides

Default: the **first tier in the chain whose set ⊇ required** (nearest superset).
`--tier <rt-core|rt-local|rt-net|rt-full>` overrides upward or downward — downward only
passes if it still satisfies §4.3 (else the error lists the missing features and the
modules that demand them).

### 4.5 `--exact` — the precise feature set via local cargo

`build --native --exact` builds a stub with exactly `required_features + bundle-zstd`:

- **Detect:** `cargo` on `PATH` and an AScript source checkout at `$ASCRIPT_SRC` whose
  `Cargo.toml` version equals this binary's `CARGO_PKG_VERSION`. Missing/mismatched →
  one specific Tier-1 error each (naming what to install/set) — never a silent fallback
  to a tier.
- **Build:** `ASCRIPT_RT=1 cargo build --release --bin ascript-rt
  --no-default-features --features <sorted set> [--target <triple>]` in `$ASCRIPT_SRC`;
  cargo's own error surfaces verbatim on failure (e.g. missing rustup target).
- **Cache:** the produced stub is content-addressed into the rt store (§5.3) keyed by
  `(version, target, sha256(sorted feature set))` so repeat builds are instant.
- **macOS:** an `--exact` darwin stub built ON a macOS host is ad-hoc signed
  (`adhoc_sign_macos`) immediately after the cargo build — before any append, per the BIN
  rule. `--exact --target *-apple-darwin` on a NON-macOS host is **rejected** with a clear
  error (the `apple-codesign` dep is macOS-host-gated, `Cargo.toml:145-146`; prebuilt
  darwin stubs — which arrive pre-signed, §6.2 — are the cross answer). Recorded
  limitation, not silent.

### 4.6 The build report (the shake-report precedent)

Every `--native` build prints to stderr (the `bundled … -> …` line stays on stdout):
chosen tier (and why: the first unsatisfied tier's missing features), stub origin + size +
sha256, required vs included features (the unused-features delta is the user's lever to
see what `--exact` would save), payload size (and compressed size when `--compress`),
shake summary (existing), and the final artifact sha256. `--report-json <path|->` emits
the same as one canonical JSON document (schema in §9.2) for CI.

## 5. Distribution: manifest, fetch, cache

### 5.1 The signed, versioned release manifest (fail-closed)

Each release publishes `rt-manifest.json` + a detached ed25519 signature:

```json
{ "schema": 1, "ascript": "0.6.0", "created": "…",
  "stubs": [ { "target": "x86_64-unknown-linux-musl", "tier": "rt-net",
               "features": ["shared","bundle-zstd","data",…],
               "sha256": "<hex>", "size": 12345678,
               "filename": "ascript-rt-0.6.0-x86_64-unknown-linux-musl-rt-net" } … ] }
```

Verification (all four required, any failure = refuse + name the reason):
1. **Signature** over the exact manifest bytes against a **compiled-in ed25519 public
   key** (`ed25519-dalek`, new optional dep behind a default-on `rt-fetch = ["net",
   "dep:ed25519-dalek"]` feature). No override env, no unsigned escape hatch — the dev
   fallbacks (§5.4) are the escape hatch.
2. **Version lock:** `manifest.ascript == CARGO_PKG_VERSION`. This is the downgrade/replay
   defense AND a correctness requirement — a stub must verify payloads with the same
   `ASO_FORMAT_VERSION` the builder emits, so stubs are version-locked to the toolchain by
   construction.
3. **Entry lookup** by `(target, tier)`.
4. **Byte pin:** the fetched stub's sha256 + size equal the manifest entry; verified
   BEFORE anything is published to the cache.

### 5.2 Fetch

Via the existing pooled `reqwest` posture (the `pkg` url-fetch precedent,
`src/pkg/fetch.rs:199-231`), from
`{base}/v{version}/{filename}` where `base` defaults to the GitHub-releases download URL
and `ASCRIPT_RT_BASE_URL` overrides it for mirrors/air-gapped registries — **the override
moves the bytes, never the trust**: the same compiled-in key verifies the same signed
manifest. `--no-fetch` / `ASCRIPT_RT_NO_FETCH=1` skips this rung entirely.

### 5.3 The content-addressed stub cache (pkg-store hygiene, reused)

Under the same root (`pkg::cache::cache_root()` — `$ASCRIPT_CACHE` first,
`src/pkg/cache.rs:26`):

```
$ASCRIPT_CACHE/rt/sha256-<hex>/ascript-rt[.exe]    # immutable, content-addressed
$ASCRIPT_CACHE/tmp/                                 # the existing staging dir (:100)
```

- **Atomic publish:** download to `tmp/`, hash, verify against the manifest pin, `chmod
  +x`, then a single `rename` into `rt/sha256-<hex>/` (the SP6 stage-then-rename rule).
- **Verify-on-load:** every cache hit is **re-hashed** before use; a mismatch (bit-rot,
  tamper) deletes the entry and falls to re-fetch. A cached stub is never trusted by path.
- The dir name IS the pin — the manifest maps `(version,target,tier) → sha256`, the cache
  maps `sha256 → bytes`. `--exact` outputs publish into the same store (§4.5).

### 5.4 The stub resolution ladder (per build, in order)

1. **`--stub <path>`** — explicit local stub (tests, air-gap, custom builds). Footer-checked
   (an existing overlay is stripped exactly as `build_native` does for `current_exe`,
   `src/lib.rs:1565-1568`); feature-verified when host-executable (§4.3).
2. **Cache hit** for the manifest-pinned `(version, target, tier)` digest (verify-on-load).
3. **Fetch** (§5.1/§5.2) — unless `--no-fetch`. Integrity failures are FATAL (fail-closed);
   pure availability failures (offline, 404, no `rt-fetch` feature) fall through.
4. **Dev sibling:** an `ascript-rt` beside `current_exe()` (the local
   `ASCRIPT_RT=1 cargo build` output) — host target only; features read via `--rt-info`
   and validated (§4.3).
5. **`current_exe()`** — host target only; today's exact behavior, plus a one-time stderr
   warning naming why every earlier rung was unavailable and that the bundle carries the
   full toolchain binary.

Cross targets stop after rung 3: no sibling/current_exe can serve a foreign triple — the
error says so and names `--exact`/`--stub`.

**Fail-closed vs fail-through, stated precisely:** *integrity* failures (bad signature,
version mismatch, checksum mismatch, corrupt cache entry that also fails re-fetch) ABORT
the build; *availability* failures fall down the ladder. A tampered artifact can never be
"recovered" by falling through to a weaker rung, because rungs 4/5 don't consult the
network at all — they use locally-built binaries.

## 6. `--target` cross builds

### 6.1 The payload is platform-independent

The `.aso`/`ASCRIPTA` payload contains bytecode, constants, and logical-path module keys —
no machine word-size, endianness (all wire fields are explicitly little-endian), or path
dependence (BNDL §3.3's lexical logical keys). The same `compile_archive` output is
appended to any target's stub. A structural test builds one program for two targets and
asserts the payload+footer bytes are identical (only the stub differs).

### 6.2 macOS: the sign-BEFORE-append rule does the work

The BIN rule (`src/lib.rs:1586-1592`): the ad-hoc signature is computed over the **clean
stub** so its `codeLimit` covers `[0, stub_len)` and the loader ignores the trailing
overlay. Consequence for RT: **prebuilt darwin stubs are signed once, at release time, in
CI** — and the builder (on ANY host OS) only ever *appends* to them, which by the rule does
not invalidate the signature. Cross-building to macOS therefore needs **no signing
machinery on the build host at all.** The only places signing still runs locally are the
host-macOS rungs that *produce* a stub: the dev sibling (signed by its own
`build_native`-equivalent flow… no — by `scripts/build-rt.sh` post-build), `--exact` on a
mac host (§4.5), and the legacy `current_exe()` path (unchanged, `src/lib.rs:1627`).
Non-macOS targets need no signing (the no-op arm, `src/bundle.rs:157-161`).

### 6.3 Un-rejecting `--target`

The `src/lib.rs:1510-1517` rejection is replaced by: validate the triple against the
published set + the ladder above; resolve a stub; append. The output filename keeps the
target's convention (`.exe` for `*-windows-*` regardless of host). The
`tests/native.rs:336-356` rejection pin is REWRITTEN (not deleted) into the new contract:
unknown triple → clear error listing supported targets; known triple without reachable
stub → the ladder-exhausted error. `--target <host triple>` is valid and equivalent to
omitting it (plus tier selection).

## 7. `--compress` — zstd payload, designed against the footer codec

### 7.1 Wire format

The 32-byte footer layout is UNCHANGED (no `FOOTER_SIZE` bump). The `reserved:u16` field
(`src/bundle.rs:46` — reserved precisely for this) becomes `flags:u16`:

- `FLAG_ZSTD = 0x0001`: the payload region is `uncompressed_len:u64 LE || <one zstd
  frame>`. The explicit length (not the zstd frame header) is the allocation clamp — the
  decoder allocates exactly `uncompressed_len` (bounded by a sanity cap mirroring the P0
  `.aso` clamp discipline), streams the frame into it, and errors unless the decoded size
  matches exactly.
- All other bits reserved-zero.

### 7.2 Versioning + the strictness matrix (loud, never misread)

Writers: `flags == 0` → write `bundle_version = 1` (**bit-identical to every existing
bundle** — reproducibility against shipped artifacts, all existing goldens/tests
untouched). `flags != 0` → `bundle_version = 2`.

Readers (new `validate_footer` contract — it gains a three-way result so "not a bundle"
and "a bundle we must refuse" are distinct; the shim already distinguishes pre- and
post-magic-confirmation failure, `src/main.rs:574-585`):

| Footer | New reader | Old (shipped) reader |
|---|---|---|
| v1, flags=0 | run (today's path) | runs (unchanged) |
| v1, flags≠0 | REFUSE loudly — v1 writers always wrote 0; this is corruption/tampering | n/a (old readers ignore the field; old writers never produce it) |
| v2, known flags | decompress + run | `from_bytes` ignores version+flags (`src/bundle.rs:66-82`) → the payload fails the `ASO\0`/`ASCRIPTA` magic check → the existing "cannot load the embedded program" error. An error, not a misread — and unreachable in practice because stubs are version-locked to the toolchain that appends (§5.1#2). Documented. |
| v2, unknown flag bits | REFUSE loudly: `this bundle uses features this runtime does not understand (flags 0x…)` | same as above |
| version > 2 | REFUSE loudly: `built by a newer ascript` | same as above |

A stub built without `bundle-zstd` that meets `FLAG_ZSTD` refuses with `this runtime was
built without compressed-bundle support` — possible only via hand-built `--exact`-style
stubs, since every published tier includes `bundle-zstd` (§3.1).

### 7.3 Cost honesty

`--compress` trades startup decompress time for artifact size; both are measured (Phase 0
adds a compressed-vs-not startup A/B to `bench/RT_SIZE_RESULTS.md`). The non-compressed
shim path is byte-identical O(1) (Gate-12 posture: the flag check is one u16 compare in
already-loaded footer bytes).

## 8. `--oci` — a loadable OCI image tarball, no Docker dependency

### 8.1 Output layout (OCI Image Layout in a tar, spec-precise)

`ascript build --oci app.as -o app.tar` (implies `--native`; composes with `--compress`,
`--target`, `--tier`, …) writes one tarball:

```
oci-layout                      {"imageLayoutVersion":"1.0.0"}
index.json                      image index (see below)
blobs/sha256/<manifest-digest>  application/vnd.oci.image.manifest.v1+json
blobs/sha256/<config-digest>    application/vnd.oci.image.config.v1+json
blobs/sha256/<layer-digest>     application/vnd.oci.image.layer.v1.tar+gzip
```

- **Layer**: a deterministic inner tar containing exactly `/app` = the bundled binary
  (mode 0755, uid/gid 0, mtime per §9.1), gzipped with `flate2::GzBuilder` (mtime 0,
  pinned compression level). `diff_id` = sha256 of the **uncompressed** inner tar; the
  layer *descriptor* digest = sha256 of the gzipped blob — the two-digest rule the OCI
  spec requires and the structural test asserts.
- **Config** (`…image.config.v1+json`): `created` fixed (§9.1), `architecture` ∈
  {amd64, arm64} mapped from the triple, `os:"linux"`, `config.Entrypoint:["/app"]`,
  `rootfs:{type:"layers",diff_ids:["sha256:<diff_id>"]}`, one fixed-timestamp `history`
  entry (`created_by:"ascript build --oci"`).
- **Manifest** (`…image.manifest.v1+json`): `schemaVersion:2`, `mediaType` explicit, the
  config descriptor, the single layer descriptor (each `{mediaType,digest,size}`).
- **index.json** (`…image.index.v1+json`): `schemaVersion:2`, one manifest descriptor with
  `platform:{architecture,os}` and the annotation
  `org.opencontainers.image.ref.name = <tag>` (`--oci-tag`, default `<stem>:latest`) — the
  annotation `docker load`/`podman load`/`nerdctl` use to name the image.

All JSON is emitted compact with hand-ordered keys (serde_json `preserve_order` is on,
`Cargo.toml:56`) so digests are deterministic.

**Tar-writer ownership (owner decision, recorded):** RT deliberately ships its **own** minimal
deterministic inner-tar writer for `--oci` — bounded by construction (exactly one fixed-name
entry, fixed timestamps/uid/gid/mode per §9.1, ~100 lines) — rather than depending on BATT's
future `std/archive`: RT precedes BATT in the track, and a CLI-side determinism-critical path
should not wait on (or churn with) a stdlib module. Unifying onto BATT's `tarcore` once it
exists is recorded as an **optional later refactor**, not a dependency in either direction.

### 8.2 No Docker at build time; validated structurally + optionally end-to-end

The structural test unpacks the tar in-process and asserts: blob filenames equal their
content digests; every descriptor's `digest`/`size`/`mediaType` is correct; `diff_id` ≠
layer digest and both verify; the inner tar holds exactly `/app` with the bundle bytes;
double-build → byte-identical `app.tar`. A separate integration test runs
`docker load -i app.tar && docker run` and is **skipped unless a working docker is
detected** (probe `docker version`), printing the skip reason — the
"where available" pattern, never a silent false-green (the structural test always runs).

### 8.3 scratch-base implication: musl only

The image has no base layers (scratch semantics), so the binary must be static:
`--oci` requires a `*-unknown-linux-musl` target. With no `--target` it defaults to
`<host-arch>-unknown-linux-musl`; a gnu/darwin/windows triple under `--oci` is rejected
with an error that names the musl equivalent. (Cross-arch images — building an arm64 image
on x86 — fall out for free: the payload is platform-independent and the musl stub is
fetched per target.)

## 9. Reproducible outputs

### 9.1 The determinism contract

Same source + same stub bytes + same flags + same `ascript` version ⇒ **bit-identical
output** — extending BNDL's deterministic archive (`shake.rs` §4.5, `ShakeReport::digest`
`:153`):

- archive bytes: already deterministic (BNDL);
- stub bytes: pinned by sha256 (manifest or `--stub`/`--exact` content address);
- footer: pure function of the two;
- zstd: single-threaded, pinned level, no content checksum — deterministic for a given
  `ascript` binary (the zstd crate is locked by that binary; cross-*version* determinism is
  explicitly NOT claimed and the docs say so);
- OCI: fixed timestamps — `1970-01-01T00:00:00Z` everywhere, or `SOURCE_DATE_EPOCH` when
  set (the reproducible-builds convention); sorted tar entries; uid/gid 0; pinned gzip.

A reproducibility battery builds everything twice (per flag combination, including
`--compress` and `--oci`) and compares bytes.

### 9.2 `--report-json`

One canonical JSON document (schema versioned, `"schema":1`) carrying: source, output
path + sha256, target, tier + `tier_source` (`selected|--tier|--exact`), required /
stub / unused-feature lists, stub `{origin, sha256, size}`, payload
`{format: aso|archive, compressed, size, uncompressed_size, sha256}`, module counts +
`shake_digest`, the embedded caps, and the `oci` sub-object when applicable. This is the
CI hook for §9.1 (compare `output_sha256` across rebuilds) and the machine twin of §4.6's
stderr report.

## 10. Testing & gates

### 10.1 Supply-chain battery (fail-closed proven, not asserted)

Hermetic (`$ASCRIPT_CACHE` tempdir per test; a local manifest + stub fixtures served from
disk via the `ASCRIPT_RT_BASE_URL` file/localhost seam): corrupt cached stub (bit-flip) →
verify-on-load refuses, entry evicted; fetched bytes ≠ manifest pin → refused, **nothing
published to cache**; manifest signature invalid / signed by another key → refused;
manifest `ascript` version ≠ toolchain (downgrade/replay) → refused; truncated stub →
refused; v2 footer with an unknown flag bit → loud refusal at launch (not clap
fall-through); compressed payload on a zstd-less stub → the §7.2 message;
`uncompressed_len` lies (too big/too small) → clean error, no over-allocation.

### 10.2 The rt-stub battery

`tests/rt_stub.rs`, env-gated on `ASCRIPT_RT_BIN` (CI builds an `ascript-rt` first via
`scripts/build-rt.sh` and exports it; locally the script does the same — the
skipped-unless-present pattern with a printed reason): bundle-onto-stub end-to-end vs
`ascript run` reference output (the `tests/native.rs` scrubbed-PATH idiom, `:94-108`);
worker parity on a stub (path (b) chunk-shipping); the §2.3(a) missing-module error text;
caps floor + `ASCRIPT_DENY` on a stub; `--rt-info` schema; bare-stub `.aso` argv run;
`--compress`ed bundle on a stub. Plus the always-on hermetic tests that need no stub:
everything in §10.1, selection/table/tiers, footer codec, OCI structural, reproducibility
(current_exe-fallback bundles exercise the whole pipeline with the full binary as stub).

### 10.3 Existing tests preserved

All of `tests/native.rs` stays green unmodified except the `--target` rejection pin, which
is rewritten to the new contract (§6.3) — coverage never shrinks (Gate 7/10).

### 10.4 Four-mode untouched — proven

RT adds no engine configuration: `vm_differential.rs` runs unmodified in both feature
configs, and the branch diff against `src/vm/`, `src/interp.rs`, `src/worker/` is
inspected to contain ONLY `#[cfg(ascript_rt)]`-gated additions (inert in every test
build — the cfg is never set under `cargo test`). Gates 15/16/18 (differential modes /
A-B perf / RSS) are N/A by construction and recorded as such; Gate 12/17 are re-affirmed
by the unchanged shim fast path (§7.3) and a startup measurement in the Phase-0 report.

### 10.5 Gate checklist (goal.md 1–14, goal-perf 15–18 where applicable)

Gates 1–4, 6–14 apply verbatim (notably: no placeholders — every narrowing in this spec is
a loud error or a recorded limitation; docs incl. NAV check; production-grade mandate —
any bug found is fixed in-branch failing-test-first). Gate 5 untouched (no checker
change). The plan's final task carries the full evidence checklist.

## 11. Scope & rejected alternatives

**In scope:** everything in §2–§9.

**Rejected:**
- **Per-program exact prebuilds** — combinatorial (2^19 feature sets × 8 targets); the
  tier chain + `--exact` cover the space at O(4) artifacts per target.
- **A `frontend` Cargo feature** instead of the `ascript_rt` cfg — breaks the
  `--no-default-features` test matrix and `CARGO_BIN_EXE_ascript` (§2.2).
- **WASM stubs** — the separate WASM spec (goal-perf track); nothing here precludes it.
- **Self-update** of stubs/toolchain — out of scope; the cache is immutable + version-
  locked instead.
- **Unsigned-manifest escape hatches** — fail-closed means no `ASCRIPT_RT_INSECURE` knob;
  dev needs are served by `--stub` and the sibling rung, which never touch the network.

**Recorded as future (owner-noted, not silent):** SBOM emission for `--oci` images;
carving tree-walker eval out of `interp.rs` if Phase 0 measures it material post-strip
(§2.3g); registry-push (`--push`) for OCI; per-tier `sysinfo` re-evaluation if its dep
weight grows.

## 12. Open risks

- **musl release matrix feasibility** — `rusqlite` (bundled C) and the rustls stack must
  cross-build under musl in CI; Task 11 includes the spike and either lands the matrix or
  records a narrowed target set with an owner note.
- **ed25519 key custody** — the release signing key lives in CI secrets; rotation requires
  a toolchain release (the pubkey is compiled in). Documented in the release runbook;
  acceptable because stubs are version-locked anyway.
- **zstd cross-version byte drift** — §9.1 scopes the determinism claim to a fixed
  `ascript` version; the repro battery would catch an unscoped claim going stale.
- **`docker load` OCI-layout support floor** — modern Docker/podman/nerdctl load OCI
  layout tarballs; the docs state the floor and `podman load` as the fallback; the
  integration test proves it where a daemon exists.
- **Feature unification in `--exact`/stub builds** — the self-dev-dependency
  (`Cargo.toml:165`) only affects test builds; stub builds compile `--bin ascript-rt`
  only and never run tests. Asserted by Task 1's build checks.

## 13. Grounding (every citation verified 2026-06-12)

`src/bundle.rs` — `BUNDLE_MAGIC:18`, `BUNDLE_FOOTER_VERSION:22`, `MIN_STUB_SIZE:27`,
`FOOTER_SIZE:31`, reserved-field doc `:44-46`, `from_bytes` (magic+length only) `:66-82`,
`validate_footer:124-141`, `adhoc_sign_macos:147-161`.
`src/lib.rs` — `build_file:931`, `compile_archive:1074`/`_with_shake:1095`,
`build_native:1504` (target rejection `:1510-1517`, payload rule `:1531-1554`, stub
strip `:1565-1568`, sign-before-append `:1586-1592` + `:1627`, offset read-back
`:1629-1634`, atomic rename `:1651-1662`), `run_aso_file:1679`, `run_embedded_aso:1696`,
`apply_ascript_deny:1711-1736`, `run_verified_aso:1744` (magic dispatch `:1757-1761`),
`run_verified_archive:1808` (+`set_worker_archive_bytes:1863`), shaker edge converter
`:1337-1356`, `set_worker_source` sites `:311,647,853,2055,2161,2303,2381`.
`src/main.rs` — `--target` arg `:84-85`, `--strip` `:73-76`, `compose_caps:311`,
`main:475`, `try_run_embedded:558-595`, shim-before-clap `:597-602`, Build arm
`:777-810`.
`src/vm/run.rs` — `load_file_module:741`, `compile_module_file:1014-1047` (incl.
`set_module_source:1045`).
`src/worker/dispatch.rs` — `build_code_slice_from_source:829-844`, static variant
`:849-863`, `resolve_worker_top_chunk:874-894`.
`src/vm/chunk.rs` — `ImportDesc:195-224`. `src/vm/aso.rs` — `ASO_FORMAT_VERSION = 27`
(`:167`). `src/vm/archive.rs` — `ARCHIVE_VERSION = 1` (`:27`).
`src/compile/shake.rs` — module doc + `ModuleNode`/`ImportEdge:59-91`,
`ReachResult:94-105`, `ShakeReport::digest:153`.
`src/pkg/cache.rs` — `cache_root:26`, `store_dir:89`, `tmp_dir:100`, `create_dirs:140`.
`src/pkg/hash.rs` — `PREFIX:23`, `asum1_tree:30`. `src/pkg/fetch.rs` —
`fetch_url`/reqwest `:199-231`.
`src/stdlib/mod.rs` — `std_module_exports:114-211`, `STD_MODULES:221-279`,
`required_cap:325`.
`Cargo.toml` — check-cfg precedent `:18-19`, `[[bin]] ascript:21-23`, serde_json
`preserve_order:56`, apple-codesign macOS-host gate `:145-146`, self-dev-dep `:165`,
`default:168`, features: fuzzgen `:172`, shared `:179`, pkg `:193`, data `:194`, binary
`:198`, log `:202`, workflow `:207`, datetime `:208`, intl `:209`, sys `:212`, crypto
`:213`, compress `:214`, sql `:215`, postgres `:219`, redis `:220`, net `:223`, http3
`:228`, tui `:231`, lsp `:235`, doc `:243`, dap `:253`, profile `:266`, sysinfo `:267`,
telemetry `:277`, ai `:285`, ffi `:294`.
`tests/native.rs` — `bin():16-18`, `serial_native:28-37`, `build_native:75-90`,
`run_bundle` scrubbed PATH `:94-108`, `--target` rejection pin `:336-356`.
Docs — `docs/content/language/bundles.md`, `docs/content/cli.md`; NAV entry
`docs/assets/app.js:27`.
