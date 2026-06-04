# SP6 — Package manager / dependency story — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (SP1–SP12). Supersedes the exploratory
> `docs/superpowers/specs/2026-06-04-sp6-package-manager-PROPOSAL.md` (which surveyed the forks and
> proposed the shape); this design records the **owner's decisions** and is the implementable contract.

**Goal:** Give AScript a first dependency story — declare third-party code in the existing
`ascript.toml`, resolve it reproducibly from **git / URL / local path** (no registry to operate),
cache it content-addressed, lock it with fail-closed integrity, and load a **bare specifier**
(`import "http"`) through that resolved set on **both engines** (bytecode VM + `--tree-walker`
oracle), byte-identical. Shape the manifest, lockfile, cache, and specifier syntax so a central
name→source registry can be added later as a **purely additive** source kind — no manifest migration.

**Architecture:** The whole feature lives in the **CLI binary** (`src/main.rs` + a new
`src/pkg/` module set) behind a new default-on, network-gated `pkg` Cargo feature, exactly like
`src/lint_config_toml.rs` keeps TOML/IO out of the interpreter core. The **only** change to the two
interpreter engines is one new branch in the import resolver — the bare-specifier branch — which
maps a resolved package name to a directory in the content-addressed store and then reuses the
**existing file-module loader** (`Interp::load_module` / `Vm::load_file_module`) verbatim, including
its `.aso` prefer/verify/recompile logic. The core never grows a network or git dependency.

**Tech stack:** Rust. CST front-end → resolver → compiler → `Chunk` → VM (default); legacy front-end
→ tree-walker (reference oracle). `toml` crate (already a non-optional dep). `git` fetched via the
`git` CLI subprocess (no git library linked). `sha2` for content addressing (already an optional dep;
pulled into the new `pkg` feature). No install scripts, ever.

---

## §0 — Grounding (verified against the tree on `feat/sp1-engine-parity`)

The PROPOSAL was written before the V12-T4 file-module work landed; these are the **current** facts
the design builds on. Every claim below was read in-tree.

1. **Imports resolve in exactly two ways today, in BOTH engines, on the same split:**
   - **Tree-walker** (`src/interp.rs:1330-1336`, `Stmt::Import`): `if source.starts_with("std/")` →
     `load_std_module` (static registry, never touches the FS); **else** →
     `resolve_import` (`src/interp.rs:945-951`: `module_dir.join(source)`, default extension `.as`) →
     `load_module` (`src/interp.rs:866`).
   - **VM** (`src/vm/run.rs:1620-1651`, `Op::Import`): the **same** `source.starts_with("std/")`
     split — std → `import_std` (`src/interp.rs:959`, the same `load_std_module`); **else** →
     `load_file_module` (`src/vm/run.rs:254`, `module_dir.join(source)`, default `.as`). The compiler
     is source-agnostic (`src/compile/mod.rs:1831` `compile_import` emits one `Op::Import` + an
     `ImportDesc` for std AND file sources; the run loop dispatches at load time).
   - So **file modules already work on both engines** (the PROPOSAL's "non-`std/` is a compile-time
     deferral" is stale — V12-T4 shipped). This is good news: the package loader reuses a complete,
     dual-engine file loader.

2. **A bare specifier is currently NOT a clean error — it silently mis-resolves.** `import "http"`
   matches neither `std/` nor a `./`/`../` prefix, so it falls into the **else** (file-module) branch
   and is joined as `module_dir/http.as`. Today that just fails to find a sibling file. SP6's new
   branch must intercept a bare specifier **before** the relative-file join, in both engines, and
   route it to the resolved store path (or emit a clear "unknown package" error). This is the **one**
   new resolver branch a package manager adds.

3. **`ascript.toml` already exists** as the project-config file (`src/lint_config_toml.rs`), today
   carrying only `[lint]`. `discover()` (`:37-55`) walks **up** from a file's dir to the FS root,
   first match wins — a free definition of "project root" (the dir of the nearest `ascript.toml`).
   The `toml` crate is **non-optional** (`Cargo.toml:36`), available under `--no-default-features`.
   This is the manifest host.

4. **`.aso` is versioned and self-contained** (`src/vm/aso.rs`): `ASO_FORMAT_VERSION = 9`
   (`aso.rs:74`), and `Chunk::from_bytes_verified` **rejects** any version mismatch (`aso.rs:367-370`).
   The file loader prefers a fresh sibling `.aso` (`vm/run.rs:292-298`, mtime rule) else recompiles
   from source — a present-but-version-mismatched `.aso` falls back to recompiling the source
   (`vm/run.rs:300-331`). **This is why SP6 ships SOURCE as the contract** (a package shipping `.aso`
   would break every consumer on the next bump); the existing prefer-`.aso`-if-fresh loader makes the
   consumer-side `.aso` cache **free** (§5.4).

5. **No cache-dir handling, no git library exist** in-tree (`grep` for `ASCRIPT_CACHE`/`XDG_CACHE`/
   `git2`/`gix` → none). Fetch is therefore via the **`git` CLI subprocess** (consistent with
   "CLI-only, feature-gated"); `sha2` (`Cargo.toml:48`) and `url` (`:40`) exist as optional deps and
   get pulled into the new `pkg` feature. No git crate is linked.

---

## §1 — Owner decisions (the design these sections implement)

These are settled; §§2–9 design to them. They resolve the PROPOSAL's §8 decision agenda.

| # | Fork | Decision |
|---|---|---|
| D1 | Distribution model | **(C) Hybrid, decentralized-first.** Resolve from git / URL / path NOW; shape manifest+lock+cache+specifier so a central name→source registry is later **purely additive** (no manifest migration). |
| D2 | Manifest home | **Extend the existing `ascript.toml`** with `[package]` + `[dependencies]`. `[lint]` untouched. |
| D3 | Resolution algorithm | **Go-style Minimal Version Selection (MVS)** — reproducible-by-default, simple resolver. Lockfile records exact resolved sources. |
| D4 | Ship source vs `.aso` | **Ship SOURCE (`.as`) as the contract; `.aso` is an optional local cache only** (because `.aso` is rejected across `ASCRIPT_FORMAT_VERSION` bumps). |
| D5 | Specifier syntax | **Bare names** (`import "http"`, registry-ready), `@scope/name` reserved now. First path segment = package, remainder = subpath. |
| D6 | Cache | `$ASCRIPT_CACHE` → XDG/platform fallback, **content-addressed** (`asum1-` = sha256 over a normalized tree). |
| D7 | Integrity | **Fail-closed** lockfile integrity on every non-path dep; re-hash on install/run, refuse on mismatch. |
| D8 | Security | **No install scripts, ever** (structural — packages are pure `.as`). Pin tag **and** commit `rev`. |
| D9 | Feature gating | New **default-on `pkg` Cargo feature**; the network/git fetch never enters `--no-default-features` core. |
| D10 | Transitive deps | **Allowed in SP6** (the MVS graph walk handles them); see §3. Path-dep transitivity included. |

---

## §2 — Manifest schema (`ascript.toml`, extended)

Two new tables added to the existing file. `[lint]` is untouched; the package parser ignores it and
the lint parser ignores the new tables (both read only their own keys — verify the lint parser does
not error on unknown tables: `parse_lint` (`src/lint_config_toml.rs:62-104`) only reads `table.get("lint")`, so unknown tables are inert — no change needed there).

```toml
[package]
name = "myapp"            # identity; lowercase, [a-z0-9-], optional @scope/ prefix. OPTIONAL for a leaf app.
version = "0.3.1"         # semver MAJOR.MINOR.PATCH. Required only to BE depended on / published.
entry = "src/main.as"     # the module a dependent binds for bare `import "myapp"`. Default: "src/main.as" then "main.as" then "<name>.as".
description = "..."       # optional metadata
license = "MIT"           # optional metadata

[dependencies]
# Decentralized-first. ONE table; the value's SHAPE selects the source kind (mutually exclusive keys).
http   = { git = "https://github.com/acme/as-http", tag = "v1.4.0" }   # git + tag (the version)
schema = { git = "https://github.com/acme/as-schema", rev = "a1b2c3d" } # git + exact commit
parse  = { url = "https://example.com/as-parse-1.2.0.tar.gz" }          # plain URL tarball
util   = { path = "../util" }                                          # local path dep (monorepo / dev)
color  = "^1.2.0"                                                      # RESERVED-FUTURE: bare semver ⇒ registry. SP6 = clean error.
```

**Schema rules (enforced by the parser, clear file-named errors like `lint_config_toml.rs`):**

- `name`: `^(@[a-z0-9-]+/)?[a-z0-9][a-z0-9-]*$`. The `@scope/` form is accepted and reserved now
  (registry-ready) but in SP6 resolves via its `[dependencies]` source like any other entry.
- `version`: strict `MAJOR.MINOR.PATCH` (the MVS comparison unit, §3). No ranges in `[package]`.
- Each `[dependencies]` value is **either** a string (reserved-future registry requirement → SP6
  emits `"bare-version dependency 'color' requires a registry, which is not available yet"`) **or** an
  inline table whose key set is exactly one of:
  - `{ git, tag }` or `{ git, rev }` (exactly one of `tag`/`rev`; `tag` recommended, `rev` for pinning),
  - `{ url }` (a tarball/zip URL),
  - `{ path }` (a local directory, relative to the manifest dir).
- Mixing source keys (e.g. `git` + `path`) → a parse error naming the offending dependency.
- `[package]` is **optional** for a leaf application (deps without being publishable). It is
  **required** (with `name` + `version` + a resolvable `entry`) for a directory to be usable **as** a
  dependency — a git/url/path dep MUST contain an `ascript.toml` with `[package]`.

**Bare-specifier → package resolution (the binding contract):**

- `import "http"` → package whose **first path segment** is `http`; bind its `entry` module.
- `import "http/router"` → the `http` package, subpath module `router` (file `router.as` relative to
  the package root) — reuses the existing relative-file loader **inside** the package's cached tree.
- `import "@acme/schema"` → the scoped package's `entry`; `import "@acme/schema/sub"` → its subpath.
- A bare specifier whose first segment is **not** in the resolved dependency set → a Tier-2 error,
  message **identical on both engines**: `unknown package 'http' — add it with 'ascript add'`
  (mirroring today's `unknown standard library module '…'`, `src/interp.rs:927-930`).

---

## §3 — Resolution (MVS) + lockfile

### Source pins vs version selection

For **git-`rev`, `url`, and `path`** deps there is no version range — the `rev`/`url`/`path` **is**
the pin; resolution is "use exactly this". For **git-`tag`** deps the tag string is the declared
version; MVS only does real work once **multiple requirements on the same package name** exist
(direct + transitive), which is possible in SP6 (D10: transitive deps allowed).

### MVS algorithm (the build list)

Implement Go's Minimal Version Selection:

1. **Build the requirement graph.** Start from the root manifest's `[dependencies]`. For each git-tag
   dep, the requirement is "name ≥ tag-version". For each transitive dep, read the **dependency's**
   `ascript.toml` `[dependencies]` (after fetch, §4) and add its requirements.
2. **Select per name = the MAXIMUM of the MINIMUMS.** For each package name, the selected version is
   the **highest** version that appears as a **requirement** across the graph (NOT the highest
   *available* tag upstream — that is the MVS reproducibility property: adding dep X can never float an
   unrelated dep Y forward; only an explicit `ascript update` raises a requirement).
3. **`rev`/`url`/`path` deps are non-versioned leaves** — each is taken as-is; if two requirements
   pin the **same name** to **different** `rev`/`url`/`path`, that is a **conflict error** (SP6 does
   not build multiple copies — single version per name, MVS-style). Report both requirers.
4. **Cycles** in the dependency graph are detected and reported (the file-module loader already
   tolerates *import* cycles at load time via the in-progress cache; a *dependency* cycle in the
   manifest graph is a resolve error with the cycle path).
5. Output: a flat **resolved set** = `{ name → resolved source + exact rev (for git) + integrity }`.

Version comparison is plain semver triple ordering (`MAJOR`, then `MINOR`, then `PATCH`); pre-release
/ build metadata are out of scope for SP6 (a tag must be `vX.Y.Z` or `X.Y.Z` — a non-conforming tag
on a git-tag dep is a clear error). This keeps the resolver to a graph walk with a per-name max — no
backtracking SAT solver.

### Lockfile — `ascript.lock`

Committed, human-diffable TOML (we already depend on `toml`). One `[[package]]` array entry per
resolved package, sorted by `name` for stable diffs.

```toml
version = 1                                      # lockfile format version (own counter, NOT .aso's)

[[package]]
name = "http"
source = "git+https://github.com/acme/as-http"
requirement = "v1.4.0"                           # what the manifest asked (tag) — for `tree`/`update`
resolved = "v1.4.0"                              # the version MVS selected
rev = "9f3c…e21"                                 # exact commit the tag pointed to at lock time
integrity = "asum1-<base64url-sha256-of-normalized-tree>"   # §5.3

[[package]]
name = "schema"
source = "git+https://github.com/acme/as-schema"
resolved = "a1b2c3d"                             # rev dep: resolved == the rev
rev = "a1b2c3d…"
integrity = "asum1-…"

[[package]]
name = "parse"
source = "url+https://example.com/as-parse-1.2.0.tar.gz"
resolved = "1.2.0"                               # from the fetched package's [package].version
integrity = "asum1-…"

[[package]]
name = "util"
source = "path+../util"                          # path dep: recorded, NO integrity (local, mutable)
```

- `ascript install` is **offline-deterministic against the lock** and **fails closed** on any
  integrity mismatch (§5).
- A **path** dep records **no integrity** — it is local and mutable, an explicitly non-reproducible
  escape hatch (same stance as Cargo path deps). Documented as such.
- The lockfile format `version` is the lock's **own** counter (starts at `1`), independent of
  `ASO_FORMAT_VERSION`.

---

## §4 — Cache, fetch, content addressing

### Cache location (resolved once, by the CLI)

```
$ASCRIPT_CACHE                          # explicit override (CI, sandboxes) — highest priority
else $XDG_CACHE_HOME/ascript            # Linux/XDG
else ~/Library/Caches/ascript           # macOS (no XDG_CACHE_HOME)
else %LOCALAPPDATA%\ascript\Cache       # Windows
else <tempdir>/ascript-cache            # last-resort fallback (warned)
```

Resolved in `src/pkg/cache.rs` with no extra dependency (read env + a small per-OS `cfg!` switch; do
not add the `dirs` crate). Layout:

```
<cache>/store/<asum-hash>/      # immutable, content-addressed package tree (the loadable package root)
<cache>/git/<host>/<path>.git/  # bare git clones, reused across fetch/update (mutable working area)
<cache>/tmp/                     # staging during fetch+hash before atomic move into store/
```

Projects do **NOT** get a `node_modules`. The resolver loads each package directly from
`store/<hash>/` keyed by the lockfile's `integrity`; the store is content-addressed and read-only at
run time, so a version is stored **once** and shared across all projects (pnpm/Bun lesson). An
optional project-local symlink farm for editor/LSP path resolution is **out of scope** for SP6
(noted as a follow-up in §10; the LSP is static-analysis only and does not currently do cross-package
go-to-def).

### Fetch (CLI-only, `pkg`-feature-gated)

- **git deps** (`{ git, tag }` / `{ git, rev }`): `git` CLI subprocess into `git/<host>/<path>.git`
  (bare clone or fetch-if-present), then `git archive`/checkout the `tag`/`rev` into `tmp/`. Record
  the exact resolved `rev` (`git rev-parse`). No working tree, no submodule scripts, no hooks run
  (a bare archive checkout — structurally no install scripts, D8).
- **url deps** (`{ url }`): download the tarball/zip via `reqwest` (already in the `net` feature; the
  `pkg` feature depends on `net` for this) into `tmp/`, extract (`flate2`/`zip`, already in
  `compress`), read its `[package].version` for the lock `resolved`.
- **path deps** (`{ path }`): no fetch — the package root **is** the local directory (resolved
  relative to the manifest dir). Not copied into the store; loaded in place. No integrity.
- After staging into `tmp/`, compute the `asum1` hash over the **normalized tree** (§5.3); the
  destination is `store/<hash>/`. If it already exists (another project fetched it), the staged copy
  is discarded — content-addressed dedup. The move into `store/` is atomic (rename).

### `.aso` composition (D4 — "free" consumer-side cache)

Cached package files are ordinary **file modules under a different root**. The existing loader already
prefers a fresh sibling `.aso`, recompiles on a stale/version-mismatched one, and version-gates via
`ASO_FORMAT_VERSION` (§0.4). So:

- A package **ships SOURCE** (`.as`) — the **contract**.
- On first load the consumer's engine compiles it; the compiled `.aso` may be cached **beside the
  source in the store** (or recompiled each run — both correct; caching is the optimization). Because
  `store/<hash>/` is content-addressed and otherwise read-only, the `.aso` cache for packages is
  written under a **writable sibling** `<cache>/aso/<asum-hash>/…` mirror (so the store stays
  immutable / hashable), and the loader is pointed there for the package root's `.aso` lookup. (If
  keeping the loader's existing same-dir `.aso` rule is simpler, the store dir may be made writable
  for `.aso` only and **excluded from the integrity hash** — §5.3 already excludes `.aso`. The plan
  picks one; both preserve "source is the hashed contract".)
- A package **MAY** also ship a prebuilt `.aso`; the loader's version check recompiles from the
  shipped source on mismatch — "ship both" degrades gracefully, source is still the contract.

---

## §5 — Security & integrity

- **D7 — fail-closed integrity.** Every non-path lock entry carries `integrity = "asum1-…"`.
  `install`/`run`/`verify` re-hash the store tree and **refuse** (non-zero exit, no execution) on
  mismatch. This is the npm/pnpm/Go stance.
- **D8 — no install scripts, ever.** AScript packages are pure `.as`; there is no `postinstall` hook
  to abuse. Fetch uses a bare git archive / tarball extract — no upstream code runs at install time.
  This is a deliberate, permanent structural advantage; keep it.
- **Pin tag AND commit.** The lock stores the human `tag`/`requirement` **and** the immutable `rev`;
  a retagged upstream is caught by the `rev` + `integrity` mismatch on the next `install`/`verify`.
- **Path deps are trusted & unhashed** (local, mutable) — explicitly the non-reproducible escape
  hatch. `verify` reports path deps as "unverified (local path)".

### §5.3 — `asum1` content hash (normalized tree)

Hash a **normalized file manifest**, not a tarball (stable across OSes, re-clones, archive formats):

1. Walk the package root; collect every file whose path ends in `.as` **plus** the package's
   `ascript.toml`. **Exclude** `.aso`, any cache dirs, VCS dirs (`.git`), and editor cruft.
2. For each included file produce `(relative-path-as-/-joined-utf8, sha256(file-bytes-verbatim))`.
3. **Sort** the pairs by relative path (byte order).
4. Feed a canonical serialization into one outer sha256: for each pair, write
   `len(path) || path || sha256_bytes` (length-prefixed so no delimiter ambiguity).
5. The result is `asum1-` + base64url(no-padding) of the 32-byte digest. The `asum1-` prefix versions
   the algorithm (rotate to sha3/blake3 later as `asum2-`, npm-SSRI style).

File bytes are hashed **verbatim** (no line-ending normalization) — packages are expected to ship
LF; a CRLF checkout would change the hash, which is acceptable (and caught) for SP6.

### Future hardening (additive, NOT in SP6)

A Go-`sum.golang.org`-style trust-on-first-use community checksum DB layers on cleanly once a caching
proxy exists; supply-chain policy (`[dependencies.policy]`, minimum release age) has manifest room.
Neither is required for SP6.

---

## §6 — The bare-specifier resolver branch (both engines — the one core change)

This is the **only** interpreter-core change. It must be **byte-identical** across the VM and the
tree-walker (the same dual-engine discipline as every other SP).

### Plumbing: a resolved package map injected by the CLI

The interpreter core must stay free of TOML/network/git. So:

- The CLI (with the `pkg` feature) does manifest discovery + MVS + lock + fetch, producing a
  **resolved package map** `name → absolute store path of that package's root` (plus each package's
  `entry`). It installs this map onto the `Interp` once, before running, via a new setter
  (e.g. `Interp::set_package_resolver(map)` storing a `RefCell<Option<PackageMap>>` — a plain
  `HashMap<String, ResolvedPkg>`, **no network types**, so the core compiles under
  `--no-default-features` with the map simply always empty there).
- A `--no-default-features` core (no `pkg` feature) has an empty map → a bare specifier hits the
  "unknown package" error (same message), since the bare language has no package manager. Correct.

### The branch (added in BOTH engines, before the relative-file join)

Specifier classification becomes a **three-way** split (today's two-way + the new middle branch):

1. `source.starts_with("std/")` → stdlib (unchanged).
2. `source` starts with `./`, `../`, or is absolute → relative file module (unchanged).
3. **otherwise → BARE PACKAGE SPECIFIER (new):**
   a. Split off the **first path segment** (`http` from `http/router`; for `@scope/name/...` the
      scope+name two segments form the package key).
   b. Look the package key up in the resolved package map.
      - **miss** → `unknown package '<key>' — add it with 'ascript add'` (Tier-2, identical message
        both engines).
      - **hit** → compute the target path: if there is no subpath, the package's `entry` file
        (absolute, inside the store root); if there is a subpath, `store-root.join(subpath)` with the
        default `.as` extension — i.e. the **existing file-module loader inputs**.
   c. Hand that absolute path to the **existing** loader:
      - tree-walker: `self.load_module(&abs_path)` (`src/interp.rs:866`), exactly as branch 2 does
        after `resolve_import`.
      - VM: `self.load_file_module(&abs_string, fault_ip, fiber)` (`src/vm/run.rs:254`), exactly as
        branch 2 does today — but with `source` rewritten to the absolute store path.
   d. **Package-internal relative imports keep working** because the loader already swaps
      `module_dir` to the loaded file's directory; a `./util` inside `http`'s entry resolves within
      `store/<hash>/…/`. (Verify the `module_dir` swap in `load_module` / `load_file_module` covers
      the store root — it does: both join the importer's dir.) A package importing **its own**
      sibling files via `./` works unchanged; a package importing **another** package via a bare
      specifier recurses through this same branch with the transitive resolved map.

### Where the branch literally goes

- **Tree-walker:** `src/interp.rs`, `Stmt::Import` arm (`:1330-1336`) — insert the bare-package
  classification between the `std/` check and the `resolve_import` fallback. Factor the classifier
  into a shared helper (`fn classify_specifier(&self, source) -> SpecifierKind`) so both engines call
  identical logic.
- **VM:** `src/vm/run.rs`, `Op::Import` exec (`:1636-1651`) — the same three-way classification before
  `load_file_module`. Use the shared classifier.
- The **compiler** (`src/compile/mod.rs:1831 compile_import`) is **unchanged** — it already emits a
  source-agnostic `Op::Import` + `ImportDesc`; classification stays a **run-time** decision (so the
  resolved map, known only at run time, drives it). This preserves "the compiler is source-agnostic".

---

## §7 — CLI surface

All under the `pkg` feature; absent the feature these subcommands are not compiled (like `Lsp` under
`lsp`). Added to the `Command` enum in `src/main.rs:14`.

```bash
ascript add <spec>          # add a dep → update manifest + lock + cache
                            #   ascript add github.com/acme/as-http@v1.4.0      (git+tag, https inferred)
                            #   ascript add https://github.com/acme/as-http@v1.4.0
                            #   ascript add ../util                              (path dep)
                            #   ascript add http        (FUTURE registry name → clean "needs a registry" error in SP6)
ascript remove <name>       # drop from manifest, re-lock
ascript install             # resolve manifest → MVS → fetch missing → WRITE/refresh lock + verify integrity
ascript install --locked    # CI: install EXACTLY from ascript.lock; fail on any drift or missing lock
ascript update [name]       # raise pin(s) to the newest satisfying tag; re-resolve (MVS) + rewrite lock
ascript lock                # (re)generate ascript.lock from the manifest without network where cached
ascript tree                # print the resolved dependency graph (name, resolved version, source)
ascript verify              # re-hash the cache store against the lock integrity entries; non-zero on mismatch
# FUTURE (only if a registry lands — NOT in SP6):
# ascript publish / ascript search
```

- `ascript run` / `ascript test` **implicitly ensure the lock is satisfied** (fetch-on-miss into the
  cache, then install the resolved map onto the `Interp`), like `cargo run`. A `--locked` flag on
  `run`/`test` makes it hermetic (no network, fail on drift) for CI.
- All package commands operate on the **nearest `ascript.toml`** (reuse `discover`,
  `src/lint_config_toml.rs:37`) and write `ascript.lock` beside it.

---

## §8 — Feature gating & crate layout (D9)

- New default-on Cargo feature **`pkg`** in `Cargo.toml` `[features]`, added to `default`.
  `pkg = ["net", "compress", "dep:sha2"]` — it needs `reqwest` (url fetch, from `net`),
  `flate2`/`zip` (extract, from `compress`), and `sha2` (content hash). `toml` is already non-optional.
  `git` is the system binary (no crate). Under `--no-default-features`, `pkg` is off → no network/git
  surface, and the bare-specifier branch always misses (empty map) → clean "unknown package" error.
- New module set **`src/pkg/`** (CLI-side, `#[cfg(feature = "pkg")]`):
  `manifest.rs` (parse `[package]`/`[dependencies]`), `lock.rs` (read/write `ascript.lock`),
  `resolve.rs` (MVS graph walk), `cache.rs` (cache dir + store layout), `fetch.rs` (git/url/path
  acquisition), `hash.rs` (`asum1` normalized-tree hash), `commands.rs` (`add`/`install`/…).
  This mirrors `src/lint_config_toml.rs` living in the binary, keeping the core TOML/IO-free.
- The **only** core (non-`pkg`) change: the `Interp::set_package_resolver` setter + the shared
  `classify_specifier` helper + the bare branch in both engines. These compile under
  `--no-default-features` (the map type is dependency-free).

---

## §9 — Test strategy (hermetic; no network in default `cargo test`)

The differential discipline holds: any program that imports a package must produce **byte-identical**
output on the VM and the tree-walker.

- **Fixture packages on local PATH deps** (primary, fully hermetic): a `tests/fixtures/pkg/` tree
  with small AScript packages (each its own `ascript.toml [package]` + `.as` files), wired as
  `{ path = … }` deps from a fixture consumer manifest. Exercises manifest parse, the resolved map,
  the bare-specifier branch (entry + subpath + scoped), transitive path deps, and the dual-engine
  byte-identical load — **with zero network**.
- **Fixture GIT repos created in a tempdir** (hermetic git, no network): tests that need the git
  source kind `git init` a bare repo in a tempdir, commit a fixture package, `git tag v1.0.0`, and
  use `{ git = "file://…", tag = "v1.0.0" }`. This exercises fetch, `rev` pinning, tag→version, and
  integrity over a real (local) git remote without touching the internet. Gated behind the `pkg`
  feature; skipped (with a clear message) if the `git` binary is absent.
- **MVS unit tests** (`src/pkg/resolve.rs`): the max-of-mins selection, transitive requirements,
  same-name `rev`/`path` conflict error, cycle detection — pure, no IO.
- **Integrity tests** (`src/pkg/hash.rs`): `asum1` is stable across re-hash, order-independent (sort),
  changes when a `.as` file changes, ignores `.aso`/`.git`. A tampered store tree fails `verify`.
- **Lockfile round-trip** (`src/pkg/lock.rs`): write→read→write is byte-stable; `--locked` install
  against a drifted lock fails closed.
- **CLI integration** (`tests/pkg.rs`, spawns the built binary like `tests/cli.rs`): `add` → manifest
  + lock updated; `install --locked` deterministic; `tree` output; `verify` pass/fail; an `import
  "fixture"` program runs byte-identical on `run` (VM) and `run --tree-walker`. **No network** — only
  path + local-`file://`-git fixtures.
- **Negative / parity tests**: bare specifier with no manifest entry → identical "unknown package"
  message + exit on both engines; a bare-version (string) dep → the clean "needs a registry" error; a
  malformed `[dependencies]` entry → a file-named parse error.
- **`--no-default-features`**: builds without `pkg`; a bare specifier → "unknown package" (empty map);
  the core compiles and the non-pkg test suite is green.

---

## §10 — The additive-registry upgrade path (why SP6 doesn't repaint later)

Turning on a central registry later is a **new source kind**, not a migration:

1. A new `[dependencies]` value shape — the **bare-version string** (`color = "^1.2.0"`), which SP6
   already parses and rejects with "needs a registry". A registry build resolves it to a concrete
   `name→source` (the registry index entry) → feeds the **same** MVS graph and the **same** lockfile
   shape (`source = "registry+https://registry.ascript.dev"` is just another `source` value).
2. The lockfile, cache store, `asum1` integrity, and the bare-specifier resolver branch are **all
   unchanged** — a registry just adds a way to *discover* the source for a name.
3. `ascript publish` / `ascript search` are new commands; nothing existing changes.

So the manifest written for SP6 keeps working verbatim after a registry lands. That is the entire
point of D1 (hybrid, decentralized-first).

---

## File-touch map (for the plan)

| Area | Files |
|---|---|
| Manifest/lock/resolve/cache/fetch/hash | NEW `src/pkg/{manifest,lock,resolve,cache,fetch,hash,commands}.rs` (`#[cfg(feature="pkg")]`) |
| Core resolver branch | `src/interp.rs` (`Stmt::Import` + shared `classify_specifier` + `set_package_resolver`), `src/vm/run.rs` (`Op::Import` classification) |
| CLI | `src/main.rs` (`Command` enum: `add/remove/install/update/lock/tree/verify`; `run`/`test` ensure-lock + `--locked`) |
| Cargo | `Cargo.toml` (new `pkg` feature in `default`; `pkg = ["net","compress","dep:sha2"]`) |
| Tests | NEW `tests/pkg.rs`; fixtures `tests/fixtures/pkg/**`; unit tests in `src/pkg/*`; differential cases in `tests/vm_differential.rs` (bare-specifier load both engines) |
| Docs | `docs/content/*` (a packages/dependencies guide page), `README.md` (CLI table), language spec module section (`docs/superpowers/specs/2026-05-29-ascript-design.md`) |

## Testing & quality bar (whole sub-project)

- **Dual-engine byte-identical** for every package-import program (VM == tree-walker == generic-VM
  where the differential harness applies); the bare-specifier branch is the only core change and must
  not diverge.
- **Both feature configs:** `cargo test` (default, with `pkg`) green AND `cargo test
  --no-default-features` green (core builds without `pkg`; bare specifier → "unknown package").
- **Clippy clean** under `--all-targets` AND `--no-default-features --all-targets`;
  `await_holding_refcell_ref` stays denied + clean (the package map setter/getter must not hold a
  `RefCell` borrow across the `.await` of `load_module`/`load_file_module`).
- **No network in default `cargo test`** — path + local-`file://`-git fixtures only.
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
  Independent per-phase review (re-read spec, re-run gates, adversarial divergence + supply-chain hunt).
