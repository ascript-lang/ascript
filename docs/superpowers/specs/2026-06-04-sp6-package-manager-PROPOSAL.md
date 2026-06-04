# SP6 — Package Manager / Dependency Registry

**Status: PROPOSAL FOR DISCUSSION — not a final spec, not an implementation plan.**

Date: 2026-06-04 · Branch context: `feat/sp1-engine-parity` · Author: design exploration

This document surveys how comparable ecosystems distribute third-party code, then proposes a
shape for AScript's first dependency story. It deliberately **surfaces the forks** (especially the
distribution model) rather than over-deciding. The owner should treat §8 as the decision agenda.

---

## 1. Where AScript is today (grounding)

There is **no package manager**. Imports resolve in exactly two ways:

1. **Stdlib namespaces** — `import * as math from "std/math"` / `import { x } from "std/math"`.
   Resolved by `load_std_module` (`src/interp.rs`) / `resolve_std_module` (`src/vm/run.rs`) from the
   static `stdlib::std_module_exports` registry. Never touches the filesystem.
2. **Relative file modules** — `import { x } from "./mod"`. Resolved by `resolve_import`
   (`src/interp.rs:945`, tree-walker) and `load_file_module` (`src/vm/run.rs:254`, VM): the specifier
   is joined onto the importer's directory (`module_dir`), the extension defaults to `.as`, and the
   loader prefers a sibling `mod.aso` (compiled bytecode) when present and at least as new as
   `mod.as`, otherwise compiles the source fresh (`src/vm/aso.rs`, `Chunk::from_bytes_verified`,
   `ASO_FORMAT_VERSION`-gated so stale bytecode is never run).

Two facts that constrain everything below:

- **The specifier string is the resolver's whole input.** Anything that is neither `std/*` nor a
  path beginning with `./` or `../` is currently unhandled. That free namespace is exactly where a
  package specifier (`pkg`, `@scope/pkg`, or a URL) must slot in — see §4.
- **`ascript.toml` already exists** as the project-config file (`src/lint_config_toml.rs`), today
  carrying only `[lint]`. Discovery walks **up** from a file to the filesystem root, project-root
  marker style, first match wins. This is the natural manifest host (§2) and gives us a free
  definition of "project root" (the dir containing the nearest `ascript.toml`).
- **`.aso` is a real, versioned, self-contained artifact** (`src/vm/aso.rs`). It already supports the
  "ship compiled, fall back to source" decision. This is a genuine asset for distribution (§4.4).

---

## 2. How comparable ecosystems do it (research)

Two archetypes, with hybrids converging in the middle.

### Central registry + semver + lockfile (Cargo, npm/pnpm, Bun, JSR)

- **Cargo:** `Cargo.toml` declares deps with semver requirements (caret `1.2.3` ⇒ `>=1.2.3, <2.0.0`
  is the default/recommended form; tilde/exact also available). The resolver picks the **newest**
  version inside the overlap of all requirements; if requirements don't overlap semver-compatibly it
  builds **multiple copies** at different majors. `Cargo.lock` pins exact resolved versions for
  reproducibility. Registry is crates.io.
  ([Cargo resolver](https://doc.rust-lang.org/cargo/reference/resolver.html),
  [specifying deps](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html))
- **npm / pnpm:** `package.json` + a lockfile (`package-lock.json` / `pnpm-lock.yaml`) holding exact
  versions **and SSRI integrity hashes** (`sha512-…`). On install the manager re-hashes the
  downloaded tarball and refuses a mismatch. **pnpm** adds a **global content-addressable store**:
  each version stored once by content hash, projects hard-link/symlink into it (huge disk + integrity
  win). pnpm 11 turns supply-chain guards on by default (`minimumReleaseAge`, blocking exotic
  transitive deps).
  ([package-lock.json](https://docs.npmjs.com/cli/v11/configuring-npm/package-lock-json/),
  [pnpm store](https://pnpm.io/settings),
  [lockfile integrity](https://medium.com/node-js-cybersecurity/lockfile-poisoning-and-how-hashes-verify-integrity-in-node-js-lockfiles-0f105a6a18cd))
- **Bun:** drop-in npm-registry-compatible, but a **binary lockfile** and a global cache at
  `~/.bun/install/cache` keyed `${name}@${version}`; installs by fastest-available syscalls
  (clonefile/hardlink) rather than pnpm-style symlinks. The takeaway: a global keyed cache + content
  reuse is the modern default. ([Bun global cache](https://bun.com/docs/pm/global-cache),
  [Bun pm](https://bun.sh/package-manager))
- **JSR (Deno's registry):** a **central** registry was added precisely because the pure-URL model
  had a reliability problem (depending on many hosts) and a duplicate-dependency problem (no shared
  semver view). Imported as `jsr:@scope/pkg`. ([JSR with Deno](https://jsr.io/docs/with/deno))

### Decentralized URL/git + checksum DB (Deno HTTP imports, Go modules)

- **Deno (HTTP imports):** originally **no central registry** — `import … from "https://…"`, no
  `package.json`, no `node_modules`; an **import map** (`deno.json` `"imports"`) maps bare specifiers
  to URLs, and `deno.lock` records exact resolved URLs + integrity hashes. Deno keeps HTTP imports but
  has publicly catalogued what went wrong (host reliability, no semver dedup) and layered JSR on top —
  a **hybrid**. ([Deno modules](https://docs.deno.com/runtime/fundamentals/modules/),
  [What we got wrong about HTTP imports](https://deno.com/blog/http-imports))
- **Go modules:** **no central registry** — module path *is* a URL (`github.com/x/y`), versions are
  VCS tags. `go.mod` lists requirements; `go.sum` holds content hashes; **Minimal Version Selection
  (MVS)** picks the *lowest* version that satisfies all requirements (maximally reproducible — adding
  a dep can't silently bump an unrelated one). A public **module proxy** (`proxy.golang.org`) + a
  **checksum database** (`sum.golang.org`) give caching + tamper-evidence on top of the decentralized
  model, configurable via `GOPROXY`/`GOSUMDB`. ([Go modules ref](https://go.dev/ref/mod),
  [MVS](https://www.ardanlabs.com/blog/2019/12/modules-03-minimal-version-selection.html))

### Synthesis

| Axis | Central (Cargo/npm/JSR) | Decentralized (Go/Deno-URL) |
|---|---|---|
| Specifier | short name `pkg` | URL / VCS path |
| Discoverability, naming | strong (search, namespace) | weak (you need the URL) |
| Infra to operate | a registry service (cost, uptime, moderation) | none (uses git hosts) |
| Reliability | single well-run host | spread across many hosts (Deno's regret) |
| Dedup / semver | one shared view | needs a proxy/sum DB to be sane (Go) |
| Day-1 cost for a tiny language | **high** | **low** |

The clear industry lesson (Deno's pivot, Go's proxy/sumdb) is that **a checksum/lock layer and a
caching proxy matter more than whether names are short**, and you can get decentralized's zero-infra
start while keeping a clean upgrade path to a registry.

---

## 3. Distribution model — FORK #1 (the big one)

Three options:

- **(A) Central registry now.** Build/operate `registry.ascript.dev`, publish flow, search, the works.
- **(B) Decentralized (git/URL) now.** Deps are git URLs (+ tag/commit). No infra. Go-style.
- **(C) Hybrid: decentralized-first, registry-shaped.** Resolve from **git/URL today**, but design the
  manifest, lockfile, cache, and specifier syntax so that a **central registry is a later, additive
  source** (a name → URL index) that requires no manifest migration.

### Recommendation: **(C) Hybrid, decentralized-first.**

Rationale for a small language:

1. **Zero infrastructure to ship.** A registry is a service with uptime, storage, abuse, and naming
   obligations the project can't yet staff. Go and early Deno prove a real ecosystem can run on git
   hosts + a lockfile. We can have working dependencies in SP6 without operating anything.
2. **AScript already has the artifacts.** `git tag` = version, `.aso`/`.as` ship as files, and we
   already have a content-hashable, versioned bytecode format. The decentralized primitives exist.
3. **The painful parts are avoidable up front.** Deno's "what we got wrong" was *host-spread* +
   *no semver dedup*; we counter both with (a) a **lockfile with integrity hashes** from day one and
   (b) a **single optional caching proxy** later (Go's playbook), not by needing a registry now.
4. **The upgrade path is clean if we design the specifier right.** If `import … from "pkg"` is a
   *bare name* resolved through a manifest entry (URL today, registry-name later), then turning on a
   registry is purely additive: a new kind of source in `[dependencies]`, same lockfile shape.

We pay one real cost: **no built-in discovery/search** until a registry exists. For a young language
that's acceptable; a curated `awesome-ascript` list bridges it.

> If the owner prioritizes ecosystem network-effects over time-to-ship, jump straight to (A). But (C)
> lets you *defer* that decision without repainting the manifest later.

---

## 4. The proposed design (under recommendation C)

### 4.1 Specifier syntax — how `import` learns a third shape

Resolution order on the specifier string (extends today's two-way split; only the third branch is new):

1. `std/*` → stdlib (unchanged).
2. starts with `./` or `../` (or is absolute) → relative file module (unchanged).
3. **otherwise → a bare *package specifier*.** Look the **first path segment** up in the resolved
   dependency set (manifest + lockfile); the remainder is a subpath into that package.

```
import * as http from "http"            // package "http", its entry module
import { Router } from "http/router"    // subpath module inside package "http"
import * as z from "@acme/schema"       // scoped package
```

Scoped names (`@scope/name`) are reserved in the grammar of names now (cheap, future-proofs a
registry namespace) even though today they resolve via a manifest URL like everything else.

This keeps spread/`std`/relative untouched and confines the new logic to one added branch in
`resolve_import` (tree-walker) and `load_file_module` (VM). Bare specifiers that aren't in the
dependency set are a **clear Tier-2 error** ("unknown package 'http' — add it with `ascript add`"),
mirroring today's "unknown standard library module".

### 4.2 Manifest — FORK #2: extend `ascript.toml` vs separate file

**Recommendation: extend the existing `ascript.toml`** (no new file). It already exists, already
defines the project root via upward discovery, and already uses the non-optional `toml` crate. Add two
tables; `[lint]` is untouched.

```toml
[package]
name = "myapp"            # identity (lowercase, registry-shaped); optional for a leaf app
version = "0.3.1"         # semver; only required to PUBLISH/be-depended-on
entry = "src/main.as"     # default module a dependent imports as bare "myapp"
description = "..."        # metadata (optional)
license = "MIT"

[dependencies]
# Decentralized-first sources. ONE table; the value's shape picks the source kind.
http   = { git = "https://github.com/acme/as-http", tag = "v1.4.0" }
schema = { git = "https://github.com/acme/as-schema", rev = "a1b2c3d" }
util   = { path = "../util" }                 # local path dep (monorepo)
color  = "^1.2.0"                             # FUTURE: bare semver ⇒ resolve via registry
```

- The `git`/`path`/(future)registry-name shapes are mutually exclusive per entry; the resolver
  dispatches on which key is present. `path` deps make monorepos/local dev work immediately and reuse
  today's relative-file loader.
- `[package]` is **optional** for a leaf application (you can have deps without being publishable).
  It's **required** to be a dependency yourself (need `name`+`version`+`entry`).
- `entry` is what `import "myapp"` binds to; subpaths resolve as files relative to the package root,
  reusing the existing file-module loader verbatim.

> Counter-argument for a separate file (`ascript.pkg.toml`): keeps tooling concerns orthogonal and
> avoids one file owning lint + deps + metadata. Rejected for now — a single project file is friendlier
> for a small language and the tables are clearly namespaced. Flagged in §8.

### 4.3 Resolution + lockfile — FORK #3: semver vs MVS

For **git/path deps** there is no version-range solving yet — a `tag`/`rev`/`path` *is* the pin. The
algorithm question (semver-newest vs MVS) only bites once a **registry with multiple publishable
versions** exists.

**Recommendation: adopt Go-style Minimal Version Selection (MVS) as the eventual algorithm**, and in
the meantime resolve git tags exactly.

Why MVS over Cargo/npm semver-newest, for a small language:

- **Reproducibility by default, with or without a lockfile.** MVS picks the lowest version satisfying
  all requirements, so adding dep X can never silently float dep Y forward. For a young ecosystem with
  few maintainers this "no spooky action" property is worth a lot.
- **A dramatically simpler resolver.** MVS is a graph walk taking the max-of-required per module — no
  backtracking SAT-style solver. Far less code to get right than Cargo/PubGrub-style resolution.
- The cost (you don't auto-get the newest patch) is mitigated by an explicit `ascript update`.

Either way, ship a **lockfile** now:

**`ascript.lock`** (TOML, human-diffable — we already depend on `toml`):

```toml
version = 1                      # lockfile format version

[[package]]
name = "http"
source = "git+https://github.com/acme/as-http"
resolved = "v1.4.0"              # the tag…
rev = "9f3c…e21"                 # …pinned to the exact commit it resolved to
integrity = "asum1-<base64-sha256-of-package-tree>"   # see §6

[[package]]
name = "util"
source = "path+../util"          # path deps recorded, no integrity (local, mutable)
```

- The lockfile is committed; `ascript install` is **offline-deterministic** against it and **fails
  closed** on any integrity mismatch.
- A path dep records no hash (it's local and mutable) — explicitly a non-reproducible escape hatch,
  same stance as Cargo path deps.

### 4.4 Cache + `.aso` — FORK #4: ship source or bytecode?

**Cache location:** a global content-addressable store, honoring an override:

```
$ASCRIPT_CACHE                         # explicit override (CI, sandboxes)
else $XDG_CACHE_HOME/ascript/packages  # Linux
else ~/Library/Caches/ascript/packages # macOS
else %LOCALAPPDATA%\ascript\Cache      # Windows
```

Layout keyed by integrity hash (pnpm/Bun lesson — a version stored once, shared across projects):

```
<cache>/store/<asum-hash>/        # immutable, content-addressed package tree
<cache>/git/<host>/<repo>/        # bare git clones for fetch/update
```

Projects do **not** get a `node_modules`. The resolver reads from the store keyed by the lockfile's
`integrity`; no per-project copy is needed because the store is content-addressed and read-only at
run time. (Optionally, a project-local `.ascript/` symlink farm for editor/LSP path resolution —
flagged in §8.)

**Ship source or `.aso`? Recommendation: ship SOURCE (`.as`), compile/cache `.aso` locally.**

- `.aso` is **not portable across `ASCRIPT_FORMAT_VERSION` bumps** by design (`src/vm/aso.rs` rejects
  a mismatch). A package shipping `.aso` would break every consumer on the next opcode/layout change.
  Shipping source means the *consumer's* toolchain compiles it, and the existing
  prefer-`.aso`-if-fresh loader transparently caches the compiled artifact next to the cached source
  (or in the store). This reuses `load_file_module`'s mtime/version logic unchanged.
- A package **MAY** also ship a prebuilt `.aso` as an optimization; the loader's existing
  version-check already falls back to recompiling from the shipped source on mismatch. So "ship both"
  degrades gracefully — but source is the **contract**.

Composition with `.aso` is therefore "free": cached package files are just file modules under a
different root, and the engine already knows how to prefer/verify/recompile `.aso` for file modules.

### 4.5 Integration touch-points (sketch, not a plan)

- One new branch in `resolve_import` / `load_file_module` for bare specifiers → store path.
- A `module_dir`-equivalent "package root" pushed when entering a package's entry module so its
  *internal* relative imports keep working (they resolve within the package's cached tree).
- Manifest + lockfile parsing live in the **CLI binary** (like `lint_config_toml.rs`), keeping the
  interpreter core free of TOML/network deps; the core only learns "given a resolved store path,
  load it" — which is the existing file loader. Network/git fetch is CLI-only and feature-gated
  (a new `pkg` Cargo feature), so `--no-default-features` core never grows a network dependency.

---

## 5. CLI surface

```bash
ascript add <spec>          # add a dep & update manifest+lock+cache
                            #   ascript add github.com/acme/as-http@v1.4.0
                            #   ascript add ../util            (path dep)
                            #   ascript add http               (FUTURE: registry name)
ascript remove <name>       # drop from manifest, re-lock
ascript install             # resolve manifest, fetch to cache, WRITE/verify lock (no-arg default)
ascript install --locked    # CI: install exactly from ascript.lock, fail on any drift
ascript update [name]       # advance pins (re-resolve tags); rewrite lock
ascript lock                # (re)generate ascript.lock without fetching network where cached
ascript tree                # print the resolved dependency graph
ascript verify              # re-hash the cache against the lockfile integrity entries
# FUTURE (only if a registry lands):
ascript publish             # package + upload [package] to the registry
ascript search <q>          # registry search
```

`ascript run` / `ascript test` implicitly ensure the lock is satisfied (fetch-on-miss), like
`cargo run`. A `--frozen`/`--locked` flag for hermetic CI.

---

## 6. Security / integrity

- **Lockfile integrity is mandatory and fail-closed.** Every non-path dep carries an `integrity`
  hash; `install`/`run` re-hash the fetched tree and **refuse** on mismatch (npm/pnpm/Go all do this).
- **Hash a normalized package tree, not a tarball.** Define `asum1-<base64(sha256)>` over a
  canonical, sorted manifest of `(relative-path, sha256(content))` pairs for the package's `.as`
  files + manifest (exclude `.aso`, caches, VCS dirs). Stable across OSes and re-clones. The `asum1-`
  prefix versions the algorithm (so we can rotate to sha3/blake3 later, npm-SSRI style).
- **Pin to a commit, not just a tag.** The lockfile stores both the human `tag` and the immutable
  `rev`; a retagged upstream is caught by `rev` + `integrity`.
- **Optional checksum DB later (Go's `sum.golang.org` model).** A trust-on-first-use community
  checksum database is an *additive* hardening once there's a proxy; not required for SP6.
- **Supply-chain guards as opt-in policy** (pnpm 11 direction): `[dependencies.policy]` could later
  gate on minimum release age or block transitive non-pinned sources. Out of scope for the first cut,
  but the manifest leaves room.
- **No install scripts, ever.** AScript packages are pure `.as`; there is no `postinstall` hook to
  abuse — a meaningful structural advantage over npm we should keep on purpose.

---

## 7. What this reuses vs. what's genuinely new

Reused as-is: `ascript.toml` discovery, the file-module loader + its `.aso` prefer/verify/recompile
logic, the `.aso` format + version gate, the relative-import `module_dir` mechanism, the `toml` crate.
Genuinely new: the bare-specifier resolution branch, a manifest `[package]`/`[dependencies]` parser,
the resolver (MVS, eventually) + `ascript.lock`, the content-addressed store + git fetch (CLI,
feature-gated), and the `ascript add/install/update/...` commands.

---

## 8. Open design questions for the owner (the decision agenda)

1. **Distribution model (FORK #1, biggest).** Accept (C) hybrid/decentralized-first? Or is ecosystem
   discoverability important enough *now* to justify operating a central registry (A) from day one?
   Everything else is downstream of this.
2. **Manifest home (FORK #2).** Extend `ascript.toml` with `[package]`/`[dependencies]` (recommended),
   or a separate `ascript.pkg.toml` to keep deps orthogonal to lint config?
3. **Resolution algorithm (FORK #3).** MVS (recommended — simpler, reproducible) vs Cargo/npm
   semver-newest (familiar, auto-patches)? This only matters once a registry exists, but it shapes the
   lockfile and `update` semantics, so decide the *direction* now.
4. **Ship source vs `.aso` (FORK #4).** Confirm "source is the contract, `.aso` is an optional local
   cache" given `.aso`'s deliberate non-portability across format-version bumps.
5. **Specifier syntax.** Bare names (`import "http"`, recommended, registry-ready) vs explicit URL
   specifiers (`import "git+https://…"`, Deno-style, zero-manifest but ugly and unstable)? Reserve
   `@scope/name` now?
6. **Versioning unit.** Git tags as the version source (Go-style `vMAJOR.MINOR.PATCH`)? Required tag
   format? How do path/`rev` deps and semver coexist?
7. **node_modules-style project copy?** Pure content-addressed store (recommended) vs a project-local
   symlink/copy farm for editor/LSP ergonomics. Does the LSP (`src/lsp/`) need on-disk resolution to
   do cross-package go-to-def, and does that force a per-project layout?
8. **Cache location + env override.** Confirm `$ASCRIPT_CACHE` → XDG/platform-cache fallback, store
   keyed by `asum1` hash.
9. **Integrity algorithm.** `asum1-` (sha256 over a normalized file tree) acceptable? Adopt a
   Go-style checksum DB later, or is the committed lockfile enough trust for the foreseeable scale?
10. **Feature gating.** New `pkg` Cargo feature (default-on) so the network/git fetch never enters the
    `--no-default-features` core — consistent with the existing stdlib feature split?
11. **Transitive deps & conflicts.** First cut: do we even allow a dependency to *have*
    `[dependencies]` (transitive graph), or restrict SP6 to direct deps and add transitivity in SP6.1?

---

## Sources

- [Cargo — Dependency Resolution](https://doc.rust-lang.org/cargo/reference/resolver.html)
- [Cargo — Specifying Dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)
- [npm — package-lock.json](https://docs.npmjs.com/cli/v11/configuring-npm/package-lock-json/)
- [Lockfile poisoning & hash integrity in Node lockfiles](https://medium.com/node-js-cybersecurity/lockfile-poisoning-and-how-hashes-verify-integrity-in-node-js-lockfiles-0f105a6a18cd)
- [pnpm — settings / content-addressable store & supply-chain guards](https://pnpm.io/settings)
- [pnpm 11.0 release notes](https://pnpm.io/blog/releases/11.0)
- [Bun — Global cache](https://bun.com/docs/pm/global-cache)
- [Bun — Package manager](https://bun.sh/package-manager)
- [Deno — Modules and dependencies](https://docs.deno.com/runtime/fundamentals/modules/)
- [Deno — What we got wrong about HTTP imports](https://deno.com/blog/http-imports)
- [JSR — Using JSR with Deno](https://jsr.io/docs/with/deno)
- [Go — Modules Reference (go.mod/go.sum, proxy, sumdb)](https://go.dev/ref/mod)
- [Go — Minimal Version Selection](https://www.ardanlabs.com/blog/2019/12/modules-03-minimal-version-selection.html)
