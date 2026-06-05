# Packages & dependencies

AScript has a built-in, **decentralized-first** package manager. You declare
third-party code in your project's `ascript.toml`, and `ascript` resolves it
reproducibly from **git**, a **URL tarball**, or a **local path** — with no
central registry to operate. Dependencies are cached content-addressed, locked
with fail-closed integrity, and loaded through a **bare specifier**
(`import "http"`) on both engines (the bytecode VM and the `--tree-walker`
oracle), byte-identical.

> The package manager ships in the default build (the `pkg` Cargo feature). A
> `--no-default-features` core build has no package manager: a bare specifier is
> a clean "unknown package" error.

## The manifest (`ascript.toml`)

The same `ascript.toml` that holds your `[lint]` config gains two tables:

```toml
[package]
name = "myapp"            # lowercase, [a-z0-9-], optional @scope/ prefix
version = "0.3.1"         # strict MAJOR.MINOR.PATCH
entry = "src/main.as"     # the module a dependent binds for `import "myapp"`
description = "..."        # optional
license = "MIT"            # optional

[dependencies]
# The value's SHAPE selects the source kind (exactly one source key).
http   = { git = "https://github.com/acme/as-http", tag = "v1.4.0" }   # git + tag
schema = { git = "https://github.com/acme/as-schema", rev = "a1b2c3d" } # git + exact commit
parse  = { url = "https://example.com/as-parse-1.2.0.tar.gz" }          # url tarball
util   = { path = "../util" }                                          # local path dep
color  = "^1.2.0"                                                      # FUTURE: needs a registry
```

- `[package]` is **optional** for a leaf application (you can declare
  dependencies without being publishable). It is **required** — with `name`,
  `version`, and a resolvable `entry` — for a directory to be usable *as* a
  dependency.
- `entry` defaults are tried in order when omitted: `src/main.as` → `main.as` →
  `<name>.as`.
- A **bare-version string** (`"^1.2.0"`) is reserved for a future central
  registry. SP6 parses it and rejects it with a clear *"requires a registry"*
  error.

## Importing a package

A **bare specifier** (no `./`, `../`, or `std/` prefix) resolves through the
dependency set:

```javascript
import { get } from "http"          // the http package's entry module
import { Router } from "http/router" // a SUBPATH module inside the http package
import { Schema } from "@acme/schema" // a scoped package's entry
```

The first path segment (or `@scope/name` for a scoped package) is the package
key; the remainder is a subpath module relative to the package root. A package's
own `./relative` imports resolve **inside** its cached tree, so packages compose
cleanly.

A bare specifier whose package is not declared is a clear error on both engines:

```
unknown package 'http' — add it with 'ascript add'
```

## Resolution: Minimal Version Selection

AScript uses Go-style **Minimal Version Selection (MVS)**: for each package name,
the selected version is the **highest version required** anywhere in the
dependency graph (direct + transitive) — *not* the highest available upstream
tag. This makes builds reproducible by default: adding one dependency never
silently floats an unrelated one forward; only an explicit `ascript update` (or a
manifest edit) raises a requirement.

- **git-tag** deps are versioned (the tag is the version).
- **git-rev / url / path** deps are non-versioned leaves, taken exactly as
  pinned. Two requirements pinning the **same name** to **different**
  rev/url/path is a conflict error that names both requirers.
- A dependency **cycle** is detected and reported with the cycle path.

## The lockfile (`ascript.lock`)

`ascript run`/`test` and `ascript install` write a committed, human-diffable
`ascript.lock` beside the manifest, sorted by name:

```toml
version = 1

[[package]]
name = "http"
source = "git+https://github.com/acme/as-http"
requirement = "v1.4.0"
resolved = "v1.4.0"
rev = "9f3c…e21"                              # the exact commit at lock time
integrity = "asum1-<base64url-sha256>"        # fail-closed integrity

[[package]]
name = "util"
source = "path+../util"                       # path deps record NO integrity
```

A **path** dep records no integrity — it is local and mutable, an explicitly
non-reproducible escape hatch (the same stance as Cargo path deps).

## The cache

Packages are stored once, content-addressed, and shared across all projects (no
per-project `node_modules`):

```
$ASCRIPT_CACHE                          # explicit override (CI, sandboxes)
else $XDG_CACHE_HOME/ascript            # Linux/XDG
else ~/Library/Caches/ascript           # macOS
else %LOCALAPPDATA%\ascript\Cache       # Windows

<cache>/store/<asum1-hash>/  # immutable, content-addressed package tree
<cache>/git/<host>/<path>.git/  # bare git clones, reused across fetch/update
<cache>/tmp/                  # staging during fetch+hash
```

The `asum1-` integrity hash is a sha256 over a **normalized file manifest**
(every `.as` file plus `ascript.toml`, sorted, length-prefixed) — stable across
OSes, re-clones, and archive formats. It deliberately excludes the compiled
`.aso` cache, `.git`, and editor dirs.

## CLI commands

```bash
ascript add <spec>     # add a dep → update ascript.toml + lock + cache
                       #   ascript add github.com/acme/as-http@v1.4.0   (git+tag, https inferred)
                       #   ascript add https://example.com/pkg-1.2.0.tar.gz
                       #   ascript add ../util                          (path dep)
ascript remove <name>  # drop from the manifest + re-lock
ascript install        # resolve → MVS → fetch missing → write/verify the lock
ascript install --locked   # CI: install EXACTLY from ascript.lock; fail on drift
ascript update [name]  # re-resolve + rewrite the lock
ascript lock           # (re)generate ascript.lock from the manifest
ascript tree           # print the resolved graph (name, resolved version, source)
ascript verify         # re-hash the store against the lock integrity (non-zero on mismatch)
```

`ascript run` and `ascript test` **implicitly ensure the lock is satisfied**
(fetch-on-miss, then load), like `cargo run`. Pass `--locked` to make them
hermetic for CI: no network, fail on any drift or integrity mismatch.

## Security model: the source is the contract

- **No install scripts, ever.** AScript packages are pure `.as`; there is no
  `postinstall` hook to abuse. Fetch uses a bare git archive / tarball extract —
  **no upstream code runs at install time**. This is a permanent, structural
  advantage.
- **Fail-closed integrity.** Every non-path lock entry carries an `asum1-`
  hash; `install`/`run --locked`/`verify` re-hash the store and refuse to run on
  a mismatch (a retagged or tampered upstream is caught by the `rev` + integrity).
- **Source, not bytecode, is shipped.** A package ships `.as` source; the
  consumer's engine compiles it (the `.aso` cache is a local optimization).
  Because the source is what is hashed, the contract survives a bytecode-format
  bump.

## The registry upgrade path

A central registry is purely additive: it would resolve a bare-version string
(`color = "^1.2.0"`) to a concrete `name → source`, feeding the **same** MVS
graph, the **same** lockfile shape, and the **same** bare-specifier import. The
manifest you write today keeps working verbatim when a registry lands.
