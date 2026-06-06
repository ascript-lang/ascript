# SP6 — Package manager / dependency story — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Declare third-party code in the existing `ascript.toml` (`[package]` + `[dependencies]`),
resolve it reproducibly from git / URL / local path via Go-style MVS, cache it content-addressed with
fail-closed integrity, and load a **bare specifier** (`import "http"`) through that resolved set on
**both engines** byte-identical. Shape everything so a central registry is later purely additive.

**Architecture:** Six phases (A–F). Everything except one resolver branch lives in the CLI binary
behind a new default-on `pkg` Cargo feature (mirroring `src/lint_config_toml.rs` keeping TOML/IO out
of the core). The single core change is a three-way import classifier (`std/` → relative → **bare
package**) added byte-identically to both `src/interp.rs` (tree-walker `Stmt::Import`) and
`src/vm/run.rs` (VM `Op::Import`), routing a resolved package to the **existing** file-module loader.
Each phase is TDD, ends green on both feature configs + clippy + the relevant differential, and gets
an independent review before the next.

**Tech Stack:** Rust. CST front-end → resolver → compiler → `Chunk` → VM (default); legacy front-end
→ tree-walker (oracle). `toml` (non-optional dep). `git` via the system binary (no git crate).
`sha2` (content hash), `reqwest`/`flate2`/`zip` (url fetch+extract) pulled into the new `pkg` feature.

**Spec:** `docs/superpowers/specs/2026-06-04-sp6-package-manager-design.md`.

**Branch:** `feat/sp1-engine-parity` (current; SP6 builds on the post-V12-T4 dual-engine file loader).

---

## Conventions for every task

- **Hermetic tests only — NO network in `cargo test`.** Package fixtures are local **path** deps
  (`tests/fixtures/pkg/**`) and **local `file://` git** repos created in a tempdir (`git init` + tag).
  A test needing the `git` binary skips with a clear message if it is absent.
- **Differential harness:** `tests/vm_differential.rs` compares `ascript::vm_run_source(src)` (spec
  VM), `ascript::vm_run_source_generic(src)` (generic VM), and `ascript::run_source_exit(src)`
  (tree-walker). For package-import cases that need a resolved map on disk, prefer the **CLI
  integration** style (`tests/pkg.rs`, spawn `env!("CARGO_BIN_EXE_ascript")` like `tests/cli.rs`)
  running the same fixture under `run` (VM) and `run --tree-walker`, asserting identical stdout+exit.
- **Per-engine manual smoke:** `cargo build` then `target/debug/ascript run X.as` (VM) vs
  `target/debug/ascript run --tree-walker X.as`.
- **Gate after each phase (paste tails):**
  `cargo test 2>&1 | tail` (0 failures all binaries, default features incl. `pkg`);
  `cargo test --no-default-features 2>&1 | tail` (0 failures; core builds without `pkg`);
  `cargo clippy --all-targets 2>&1 | tail` AND `cargo clippy --no-default-features --all-targets 2>&1 | tail` (clean);
  `grep await_holding_refcell_ref Cargo.toml` (still `deny`).
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** weaken a differential assertion or edit a passing tree-walker test to make the VM pass. A
  divergence on a package-import program = fix the classifier/loader root cause.
- **Core-purity invariant:** `src/pkg/*` is `#[cfg(feature = "pkg")]` and CLI-only; the interpreter
  core (`src/interp.rs`, `src/vm/*`, `src/value.rs`) gains NO network/git/toml dependency — only the
  dependency-free package-map setter + the shared classifier.

---

## Phase A — Manifest schema (`[package]` + `[dependencies]` parse)

**Files:** NEW `src/pkg/mod.rs` + `src/pkg/manifest.rs` (`#[cfg(feature="pkg")]`); `Cargo.toml` (new
`pkg` feature). Tests inline in `manifest.rs`.

### Task A1: add the `pkg` feature + module skeleton

- [ ] **Step 1 — `Cargo.toml`.** Add to `[features]`: `pkg = ["net", "compress", "dep:sha2"]` and add
  `"pkg"` to the `default` list. (`toml` is already non-optional; `sha2`/`reqwest`/`flate2`/`zip`
  already exist as optional deps — `pkg` just turns the needed ones on.)
- [ ] **Step 2 — Skeleton.** Create `src/pkg/mod.rs` declaring `#[cfg(feature="pkg")] pub mod manifest;`
  (and stubs for `lock`/`resolve`/`cache`/`fetch`/`hash`/`commands` added in later phases). Wire
  `#[cfg(feature="pkg")] mod pkg;` into `src/main.rs`.
- [ ] **Step 3 — Build:** `cargo build` (default) and `cargo build --no-default-features` both compile
  (the latter excludes `src/pkg`). Commit: `chore(pkg): add default-on pkg Cargo feature + module skeleton`.

### Task A2: parse `[package]` + `[dependencies]`

- [ ] **Step 4 — Read** `src/lint_config_toml.rs` (the model: `toml::Table` parse, file-named errors,
  `discover()` upward walk). Confirm `parse_lint` only reads `table.get("lint")` (so unknown tables
  are inert — the package parser is orthogonal).
- [ ] **Step 5 — Failing tests** (inline in `manifest.rs`): a `Manifest` parsed from a TOML string with
  `[package]` (name/version/entry/description/license) + `[dependencies]` covering each source shape
  (`{git,tag}`, `{git,rev}`, `{url}`, `{path}`, and a bare-version string). Assert: valid name regex,
  strict `MAJOR.MINOR.PATCH` version, the source-kind enum is selected by the present key, mixed keys
  (`git`+`path`) is a file-named error, a bare-version string parses into a `RegistryRequirement`
  variant (resolved later). Run → fail (no parser yet).
- [ ] **Step 6 — Implement** `manifest.rs`: a `Manifest { package: Option<PackageMeta>, dependencies:
  IndexMap<String, DepSource> }`; `DepSource` enum `{ Git{url,pin: GitPin}, Url{url}, Path{path},
  Registry{req} }` with `GitPin::Tag(String)|Rev(String)`. Parse via `toml::Table`, validate name
  (`^(@[a-z0-9-]+/)?[a-z0-9][a-z0-9-]*$`) and version triple, emit clear `ascript.toml: <name>: …`
  errors. Add a `discover()`-backed `load_nearest(file) -> Result<(PathBuf, Manifest), String>`
  reusing the lint-config upward-walk pattern (factor the walk if cheap, else duplicate the small loop).
- [ ] **Step 7 — Run** the manifest tests → green. Phase-A gate. Commit:
  `feat(pkg): parse ascript.toml [package] + [dependencies]`.

---

## Phase B — Cache layout + content hash (`asum1`)

**Files:** NEW `src/pkg/cache.rs`, `src/pkg/hash.rs`. Tests inline.

### Task B1: cache directory resolution

- [ ] **Step 1 — Failing test** (`cache.rs`): `cache_root()` honors `$ASCRIPT_CACHE` first; with it
  unset, returns a per-OS path (use a test that sets `ASCRIPT_CACHE` to a tempdir and asserts the
  store/git/tmp subdir helpers join under it). Run → fail.
- [ ] **Step 2 — Implement** `cache.rs`: `cache_root()` = `$ASCRIPT_CACHE` else a `cfg!`-per-OS
  fallback (`$XDG_CACHE_HOME/ascript` / `~/Library/Caches/ascript` / `%LOCALAPPDATA%\ascript\Cache` /
  tempdir last-resort) — **no `dirs` crate**. Helpers `store_dir(hash)`, `git_dir(url)`, `tmp_dir()`,
  `create_dirs()`. Commit: `feat(pkg): content-addressed cache layout + $ASCRIPT_CACHE override`.

### Task B2: `asum1` normalized-tree hash

- [ ] **Step 3 — Failing tests** (`hash.rs`): `asum1_tree(dir)` over a fixture dir is (a) stable
  across two calls, (b) unchanged when file enumeration order differs (sort), (c) **changes** when a
  `.as` file's content changes, (d) **ignores** a sibling `.aso` and a `.git/` dir, (e) the output
  starts `asum1-` and round-trips base64url. Run → fail.
- [ ] **Step 4 — Implement** `hash.rs` per spec §5.3: walk root, include `*.as` + `ascript.toml`,
  exclude `.aso`/`.git`/cache dirs; per file `(rel-path-/-joined, sha256(bytes))`; sort by path;
  outer sha256 over length-prefixed `len||path||digest`; emit `asum1-` + base64url(no-pad). Use
  `sha2` + `base64` (both available under `pkg`). Commit: `feat(pkg): asum1 normalized-tree integrity hash`.

---

## Phase C — Fetch (path, git, url)

**Files:** NEW `src/pkg/fetch.rs`. Tests inline + a hermetic-git helper.

### Task C1: path + git fetch

- [ ] **Step 1 — Failing tests** (`fetch.rs`): (a) a **path** dep resolves to the local dir in place
  (no copy, no integrity); (b) a **git** dep against a **local `file://` bare repo** created in a
  tempdir (`git init --bare`, a work clone that commits a fixture package + `git tag v1.0.0`, push)
  fetches, checks out the tag into `tmp/`, returns the exact `rev` (`git rev-parse`), and stages into
  `store/<asum1>/`. Skip-with-message if the `git` binary is absent. Run → fail.
- [ ] **Step 2 — Implement** the git/path arms in `fetch.rs`: path → return the absolute local dir;
  git → `git` subprocess (bare clone/fetch into `git_dir`, `git archive`/checkout `tag`/`rev` into
  `tmp/`, `git rev-parse` for the resolved rev), then `asum1_tree` the staged tree and atomic-rename
  into `store/<hash>/` (skip if it already exists — dedup). **No hooks / no submodule scripts** (bare
  archive). Commit: `feat(pkg): fetch path + git (local file:// hermetic) deps into the store`.

### Task C2: url fetch

- [ ] **Step 3 — Failing test** (`fetch.rs`, hermetic): serve a tarball from a local file path (or a
  tiny in-process `tokio` server bound to `127.0.0.1`) — extract, read `[package].version`, stage +
  hash into the store. (Prefer a local-file URL if `reqwest` supports `file://`; else a loopback
  server with no external network.) Run → fail.
- [ ] **Step 4 — Implement** the url arm: download (`reqwest`, from `net`), extract (`flate2`/`zip`,
  from `compress`) into `tmp/`, parse the package's `ascript.toml` `[package].version`, hash + stage.
  Phase-C gate. Commit: `feat(pkg): fetch url tarball deps (hermetic loopback test)`.

---

## Phase D — Bare-specifier resolution in BOTH engines (the one core change)

**Files:** `src/interp.rs` (shared `classify_specifier`, `set_package_resolver`, `Stmt::Import`
branch), `src/vm/run.rs` (`Op::Import` classification). NEW `tests/pkg.rs` (CLI integration). The
package map injected here is a **dependency-free** `HashMap<String, ResolvedPkg>` so the core compiles
under `--no-default-features`.

### Task D1: the dependency-free package-map plumbing

- [ ] **Step 1 — Read** `src/interp.rs:1330-1336` (`Stmt::Import`), `:945-951` (`resolve_import`),
  `:866` (`load_module`); `src/vm/run.rs:1620-1651` (`Op::Import`), `:254` (`load_file_module`).
  Confirm both engines split on `source.starts_with("std/")` then fall through to a relative join, and
  that a bare specifier currently mis-resolves to `module_dir/<name>.as`.
- [ ] **Step 2 — Implement** in `src/interp.rs`: a `PackageMap = HashMap<String, ResolvedPkg>` where
  `ResolvedPkg { root: PathBuf, entry: PathBuf }` (plain std types, no `pkg`/network deps); a
  `RefCell<Option<PackageMap>>` field on `Interp` + `pub fn set_package_resolver(&self, map: PackageMap)`.
  A shared `fn classify_specifier(&self, source: &str) -> SpecifierKind { Std | Relative(PathBuf) |
  Package{key, target: PathBuf} | UnknownPackage(String) }` that does the three-way split (key = first
  segment, or scope+name for `@scope/...`; `target` = entry if no subpath, else `root.join(subpath)`
  with default `.as`). Build (`cargo build` + `--no-default-features`). Commit:
  `feat(core): dependency-free package resolver map + shared specifier classifier`.

### Task D2: wire the branch into both engines

- [ ] **Step 3 — Failing CLI tests** (`tests/pkg.rs`): a fixture consumer (`tests/fixtures/pkg/app/`)
  with `ascript.toml` declaring a `{ path = "../lib" }` dep and a program `import "lib"` (entry) +
  `import "lib/util"` (subpath) + a scoped `@scope/x` path dep. Assert the program runs **byte-identical**
  under `ascript run` (VM) and `ascript run --tree-walker`, and that an `import "missing"` yields the
  identical `unknown package 'missing' — add it with 'ascript add'` message + exit on both engines.
  (For now, before the CLI `install` exists, the test can pre-populate the resolver by pointing
  `run` at a manifest whose path deps need no fetch — path deps load in place; D2 may temporarily
  build the map directly from the manifest's path deps. The git/url map population is finished in
  Phase E via the resolver.) Run → fail.
- [ ] **Step 4 — Implement** the bare branch in BOTH engines using `classify_specifier`:
  `Std` → unchanged; `Relative(p)` → unchanged (`load_module`/`load_file_module`); `Package{target}` →
  call the **same existing loader** with `target` (tree-walker `load_module(&target)`; VM
  `load_file_module(&target.to_string_lossy(), fault_ip, fiber)`); `UnknownPackage(key)` → the Tier-2
  error, identical message both engines. **Invariant:** do not hold the `package_resolver` `RefCell`
  borrow across the loader `.await` (clone the `ResolvedPkg`/target out first).
- [ ] **Step 5 — CLI wiring:** in `src/main.rs` `Command::Run`/`Test`, before running, build the
  package map for path deps from the nearest manifest and call `interp.set_package_resolver(map)` (full
  fetch+lock integration lands in Phase E; this step makes path deps work end-to-end and proves the
  dual-engine branch). Under `--no-default-features` (no `pkg`) the map stays empty → "unknown package".
- [ ] **Step 6 — Run** `tests/pkg.rs` → byte-identical both engines; manual smoke. Phase-D gate.
  Commit: `feat(core): bare-specifier import branch (path deps) — byte-identical both engines`.

---

## Phase E — MVS resolution + lockfile + the resolver-driven map

**Files:** NEW `src/pkg/resolve.rs`, `src/pkg/lock.rs`; `src/main.rs` (wire resolve→fetch→map into
`run`/`test`). Tests inline + `tests/pkg.rs`.

### Task E1: lockfile read/write

- [ ] **Step 1 — Failing tests** (`lock.rs`): a `Lockfile { version:1, packages: Vec<LockEntry> }`
  serializes to TOML `[[package]]` entries sorted by name; write→read→write is byte-stable; a path
  entry omits `integrity`; a git entry carries `requirement`/`resolved`/`rev`/`integrity`. Run → fail.
- [ ] **Step 2 — Implement** `lock.rs` per spec §3 (`source = "git+…"`/`"url+…"`/`"path+…"`,
  base64url integrity, own `version = 1` counter). Commit: `feat(pkg): ascript.lock read/write (stable, sorted)`.

### Task E2: MVS graph walk

- [ ] **Step 3 — Failing unit tests** (`resolve.rs`, pure/no-IO via an injectable "read a package's
  deps" callback): max-of-mins selection across direct+transitive git-tag requirements; a non-versioned
  (`rev`/`url`/`path`) leaf taken as-is; **same-name conflicting `rev`/`path` → conflict error naming
  both requirers**; **cycle detection** with the cycle path; a bare-version (Registry) requirement →
  the "needs a registry" error. Run → fail.
- [ ] **Step 4 — Implement** `resolve.rs`: build the requirement graph from the root manifest, fetch
  each dep (via `fetch.rs`), read the fetched package's `[dependencies]` for transitive requirements,
  select per name = highest required version (semver triple), detect conflicts + cycles, output the
  flat resolved set `{name → ResolvedPkg + lock metadata}`. Commit: `feat(pkg): Go-style MVS resolver (transitive, conflict + cycle errors)`.

### Task E3: end-to-end ensure-lock on `run`/`test`

- [ ] **Step 5 — Failing CLI test** (`tests/pkg.rs`): a fixture consumer with a **local `file://`
  git** dep (tempdir, tagged) — `ascript run` fetches into the cache, writes `ascript.lock`, builds
  the resolver map (git + path), and the program runs byte-identical VM vs `--tree-walker`. A second
  `ascript run --locked` is offline-deterministic against the written lock. Run → fail.
- [ ] **Step 6 — Implement** in `src/main.rs`: `Command::Run`/`Test` "ensure lock satisfied" =
  load manifest → MVS resolve (fetch-on-miss, unless `--locked`) → write/verify lock → assemble the
  `PackageMap` (now incl. git/url store paths) → `set_package_resolver`. `--locked` = no network, fail
  on drift/missing lock. Phase-E gate. Commit: `feat(pkg): run/test ensure ascript.lock (MVS resolve + fetch + --locked)`.

---

## Phase F — CLI commands + integrity verify + docs + holistic review

**Files:** NEW `src/pkg/commands.rs`; `src/main.rs` (`Command` enum); `docs/content/*`, `README.md`,
language spec module section. Tests in `tests/pkg.rs`.

### Task F1: `add` / `remove` / `install` / `update` / `lock` / `tree` / `verify`

- [ ] **Step 1 — Failing CLI tests** (`tests/pkg.rs`, hermetic path + `file://`-git fixtures):
  `add ../lib` → manifest `[dependencies]` + `ascript.lock` updated; `add <file://repo>@v1.0.0` →
  git+tag entry, lock has `rev`+`integrity`; `remove lib` → dropped + re-locked; `install` →
  resolves+fetches+writes lock; `install --locked` against a drifted lock fails closed; `update` →
  raises the pin to a newer tag + rewrites lock; `lock` → regenerates without network where cached;
  `tree` → prints the resolved graph (name, resolved, source); `verify` → passes on a clean store and
  **fails non-zero** on a tampered store tree. Run → fail.
- [ ] **Step 2 — Implement** `commands.rs` + add the `Command` variants in `src/main.rs:14`
  (`#[cfg(feature="pkg")]`, like `Lsp` under `lsp`): `Add{spec}`, `Remove{name}`, `Install{locked}`,
  `Update{name:Option}`, `Lock`, `Tree`, `Verify`. `add` infers `git+https` from a `github.com/…`
  spec, `https://…@tag`, a `../path`, or a bare name (→ "needs a registry"). Each operates on the
  nearest `ascript.toml` (reuse `discover`) and writes `ascript.lock` beside it. `verify` re-hashes
  every non-path store entry against the lock `integrity` (fail-closed). Run → green.
- [ ] **Step 3 — Commit:** `feat(pkg): add/remove/install/update/lock/tree/verify CLI commands`.

### Task F2: docs

- [ ] **Step 4 — Write** a `docs/content/` packages/dependencies guide page (manifest schema, the four
  source kinds, MVS + `ascript.lock`, the `add/install/update/lock/tree/verify` commands, the
  cache/`$ASCRIPT_CACHE`, the "source is the contract / no install scripts" security model, the
  bare-specifier import form). Update `README.md`'s CLI table + the language spec module section
  (`docs/superpowers/specs/2026-05-29-ascript-design.md`). Verify documented snippets against the
  built binary. Commit: `docs: package manager guide + CLI table + spec module section`.

### Task F3: holistic gate + review

- [ ] **Step 5 — Full gate set** both feature configs + clippy both + the whole `tests/pkg.rs` +
  differential. Confirm `--no-default-features` builds without `pkg` and a bare specifier yields
  "unknown package".
- [ ] **Step 6 — Independent review:** re-read the spec; re-run gates; adversarial hunt over —
  bare-specifier subpath + scoped names, package-internal `./` imports resolving inside the store,
  transitive path+git mix, MVS conflict/cycle messages, integrity tamper detection, `--locked`
  hermeticity, and the dual-engine byte-identical load. **Supply-chain check:** confirm no upstream
  code runs at fetch (bare archive, no hooks/submodule scripts), and `verify` is genuinely
  fail-closed. Fix any divergence/finding at the root.
- [ ] **Step 7 — Final commit** if review surfaced fixes; else the phase is complete.

---

## Self-review (author)

**Spec coverage:** §2 manifest → Phase A; §4 cache+hash → Phase B; §4 fetch (path/git/url) → Phase C;
§6 bare-specifier branch both engines → Phase D; §3 MVS+lockfile + run/test ensure-lock → Phase E;
§7 CLI commands + §5 integrity verify + §10 registry path (the reserved bare-version error is parsed
in A, errored in E2) + docs → Phase F. All spec sections mapped.

**Decision coverage:** D1 hybrid (path/git/url now, registry additive — A parses + E2 errors the
reserved string); D2 extend `ascript.toml` (A); D3 MVS (E2); D4 source-is-contract / `.aso` cache
(loader reuse in D, no `.aso` shipped); D5 bare names + `@scope` reserved (A regex + D classifier);
D6 `$ASCRIPT_CACHE`/XDG + `asum1` (B); D7 fail-closed integrity (B hash + E1 lock + F verify); D8 no
install scripts (C bare archive); D9 `pkg` feature default-on, core stays dependency-free (A1 + D1);
D10 transitive deps (E2). Complete.

**Core-purity:** the ONLY non-`pkg` change is the dependency-free package map + shared
`classify_specifier` + the branch in both engines (D) — verified to compile under
`--no-default-features` (map stays empty → "unknown package"). `src/pkg/*` is entirely
`#[cfg(feature="pkg")]`, CLI-side, mirroring `src/lint_config_toml.rs`.

**Hermeticity:** no test touches the network — path deps + local `file://` git repos in a tempdir +
loopback/local-file url fetch. Git-dependent tests skip cleanly without the `git` binary.

**Placeholder scan:** none. The one deferred-to-implementer detail is the exact `.aso`-for-packages
write location (spec §4 offers two byte-equivalent options — writable sibling mirror vs store dir
writable-for-`.aso`-only-and-excluded-from-hash; the implementer picks one, both keep source as the
hashed contract). Differential-helper names match the existing `tests/vm_differential.rs` / `tests/cli.rs`
conventions (read the files for the actual helpers).

**Version-counter consistency:** the lockfile `version` counter (starts `1`) is the lock's OWN, NOT
`ASO_FORMAT_VERSION` (currently 9) — SP6 touches neither `.aso` nor its version.
