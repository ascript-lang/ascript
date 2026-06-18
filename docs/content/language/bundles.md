:::eyebrow Language

# Self-contained bundles

A single-file script compiles to a single `.aso` and is already self-contained. A program that
`import`s sibling files or packages is not — the imports resolve from disk at run time. AScript
closes that gap: both `ascript build` and `ascript build --native` embed a program's **whole
reachable module graph** (tree-shaken) and its **capability set** into one artifact. The result
runs with **no source tree present**.

```bash
ascript build app.as -o app.aso        # bytecode + every imported module, in one .aso
ascript build --native app.as -o app   # the whole runtime + the program, in one executable
```

The `--native` form is **bundling, not ahead-of-time compilation**: the embedded VM still
interprets the bytecode. You get a standalone executable, not native machine code. (See
[Compilation & runtime](../runtime) for the `.aso`/VM model.)

> **Standard library stays native.** `std/*` modules are written in Rust and linked into the
> runtime. They are *never* embedded or tree-shaken — only your `.as`/package modules are. A
> `--native` binary carries the std implementation because it carries the whole runtime; a `.aso`
> archive relies on the `ascript` runtime that runs it, exactly as a bare `.aso` does today.

## What gets embedded — the module archive

A program with **no** file or package imports stays a bare single-chunk `.aso` (unchanged). As
soon as it has imports, `build` produces a versioned **module archive** (`ASCRIPTA`) instead: a
manifest plus a table of per-module, individually-verified bytecode chunks.

- Modules are keyed by a **machine-independent logical path** — derived from import specifiers
  relative to the entry file's directory, never from an absolute build-machine path. The same
  archive runs unchanged on any machine.
- **Circular and diamond imports** work: a module reached two ways is archived once, and cycles
  terminate.
- Module-load **side effects** and **once-only** semantics are preserved exactly as a disk run —
  the same top-level statements run, in the same order, exactly once.

At run time the embedded modules are consulted **before** disk, so a bundled program never needs
its sources. (If a key is absent from the archive — a partial/dev build — the loader falls
through to the on-disk path.)

> **A built artifact is frozen.** Because `build` embeds the whole module graph, a `.aso`/native
> bundle is a self-contained snapshot taken at build time. Editing a sibling `.as` afterward has
> **no effect** until you rebuild — the run resolves the import from the embedded archive, not the
> (now-newer) source on disk. For a live-edit dev loop, run the source directly with `ascript run
> app.as`, which always recompiles; reach for `build` when you want a portable, reproducible
> artifact. (This is the intended shift from the older model where a `.aso` referenced its imports
> externally and recompiled stale dependencies.)

## Tree-shaking

Embedding the whole import graph naively would bloat artifacts with code that is never used, so
the builder drops unreachable top-level declarations from imported library modules.

- **The entry module is kept whole** — every top-level statement runs on import, so all of them
  are reachable roots.
- A library module's unused, inert top-level `fn` / `class` / `enum` / `interface` / `const` is
  **dropped**. A **reachable class is kept whole** (its methods dispatch dynamically), along with
  its superclass chain, implemented interfaces, field types, and referenced enum variants.
- Side-effectful top-level statements (a bare call, a `let x = compute()`) are **always kept** —
  dropping them would change what runs on import.

Shaking only changes *which bytes are produced*, never observable behavior — a shaken archive is
guaranteed to produce byte-identical output to the same program run unshaken from disk.

### Named vs. namespace imports

Named imports shake **per binding**: `import { greet } from "./util"` marks exactly `greet`
reachable and shakes the rest of `util`.

A namespace import (`import * as m from "./util"`) shakes per binding **only** when every use is a
static `m.foo` read or `m.foo(args)` call. If `m` is indexed dynamically (`m[key]`) or **escapes**
as a value (returned, stored, passed as an argument), the analysis can no longer prove which
exports are used, so it conservatively **pins the whole module** — that one module, with the
reason recorded in the build report. The rule is one-directional: the shaker may over-include, it
never under-includes.

### Reading the build report

`build` prints a tree-shaking report to **stderr** (stdout stays clean), listing the dropped
declarations per module and any pinned modules with the reason and `file:line:col`:

```text
tree-shaking: dropped 1 declaration(s) across 1 module(s); 0 module(s) pinned
  util.as: dropped 1 — farewell
```

A pinned module names why it could not be shaken — your lever to refactor toward named imports
and shrink the artifact:

```text
tree-shaking: dropped 0 declaration(s) across 0 module(s); 1 module(s) pinned
  kept all exports of 'dynutil.as' — namespace 'm' is indexed/escapes at dynapp.as:3:7
```

The manifest also carries a reproducible 32-byte **digest** of the shake report (a deterministic
function of the source), for tooling that wants to compare builds.

## Embedded capabilities

[Capabilities](../stdlib/caps) (`fs` / `net` / `process` / `ffi` / `env`) restrict what a program
may do. With bundles, those restrictions travel **inside the artifact**:

```bash
ascript build --native --deny net app.as -o app
./app    # net is denied at launch — no flag, no sidecar, no rebuild
```

`build` composes the capability set from the **same** source a normal run uses — the CLI flags
(`--deny` / `--sandbox` / `--deny-net` / `--deny-fs`, identical grammar to `run`) plus the nearest
`ascript.toml [capabilities]` table — and serializes it into the archive manifest. At launch the
bundle reads it and enforces it, exactly like a live `--deny` run.

Composition is **monotone** — `build`-time caps ∩ launch-time `--deny` ∩ `ASCRIPT_DENY`. No layer
can re-grant a denied capability; each can only subtract further. A program developed and tested
under `--deny net` therefore ships sandboxed, with no way to silently widen its caps later.

### Restricting a bundle at launch — `ASCRIPT_DENY`

A bundled binary forwards all argv to the *program*, so caps are not overridable via flags. To
tighten a pre-built bundle without rebuilding, set **`ASCRIPT_DENY`** — a comma-separated list of
cap names, same vocabulary as `--deny`:

```bash
ASCRIPT_DENY=fs,net ./app    # deny fs and net on top of whatever was embedded
```

It can **only subtract** — never grant a cap the embedded set denied. An unknown cap name is a
clear startup error, not a silent no-op:

```text
error: ASCRIPT_DENY: unknown capability 'bogus' (expected one of: fs, net, process, ffi, env)
```

This is the ops escape hatch: deploy one pre-built bundle into a tighter sandbox per environment.

> **Security note — the macOS signing boundary.** On macOS a `--native` bundle is ad-hoc signed,
> and the embedded payload (the bytecode archive **and** its capability set) is appended *after*
> the signature. The embedded caps are therefore **not covered by the signature** — they are
> tamper-**evident**, not tamper-**proof**. This is an accepted boundary, not an oversight:
> lowering one's own caps is not an attacker's goal, and anyone who can rewrite the binary's
> overlay can replace the entire payload anyway (at which point the signature no longer validates).
> Treat the embedded caps as a faithful default, not a sandbox you can hand to a hostile party.

## A worked example

Two files — `app.as` imports one function from `util.as`, which also defines an unused export:

```ascript
// util.as
export fn greet(name: string): string {
  return "Hello, " + name + "!"
}

// never imported by app.as — the tree-shaker drops it
export fn farewell(name: string): string {
  return "Goodbye, " + name + "."
}
```

```ascript
// app.as
import { greet } from "./util"

print(greet("world"))
```

Build a sandboxed native binary and run it from an empty directory:

```bash
$ ascript build --native --deny net app.as -o app
tree-shaking: dropped 1 declaration(s) across 1 module(s); 0 module(s) pinned
  util.as: dropped 1 — farewell
bundled app.as -> app (44300728 bytes)

$ cd /tmp/empty && ./app          # no util.as in sight
Hello, world!

$ ASCRIPT_DENY=fs ./app           # tighten further at launch
Hello, world!
```

The report goes to stderr (`util.farewell` was dropped), `bundled … -> app` goes to stdout, and
the binary runs anywhere with `net` denied and `fs` deniable at launch — sources and all.

## Runtime stubs and the resolution ladder

A `--native` bundle is a **runtime stub** with the compiled program appended. The stub the
payload is appended onto is resolved through a five-rung ladder, in order:

1. **`--stub <path>`** — an explicit local `ascript-rt` stub (tests, air-gaps, custom builds,
   cross targets). If the stub is itself a bundle, its overlay is stripped first; when the
   stub runs on this host its feature set is verified via `--rt-info` and a **tier-insufficient
   stub is rejected** (it names the missing feature and the importing module).
2. **Cache** — a previously fetched/verified stub in the content-addressed store.
3. **Fetch** — download the per-target stub against the signed, version-locked release
   manifest (skipped by `--no-fetch` / `ASCRIPT_RT_NO_FETCH=1`). An **integrity** failure
   (bad signature, checksum, or version) ABORTS the build; a pure **availability** failure
   (offline, 404) falls through.
4. **Dev sibling** — an `ascript-rt` next to the running toolchain binary (host target only).
5. **`current_exe()`** — the full toolchain binary itself, with a one-time warning that the
   bundle carries the whole toolchain.

The default host build with none of these flags ends at rung 5 (today's behavior). A smaller
stub trims the bundle to just the runtime; the build report shows the chosen tier and the
unused-feature delta. **Integrity failures are fatal; availability failures fall through** — a
tampered fetched stub is never "recovered" by falling back to a weaker rung, because rungs 4/5
are local binaries that never touch the network.

### Stub tiers

The published stubs come in four cumulative tiers, selected automatically from the program's
imports (the nearest tier whose feature set is a superset of what the program needs). A
pure-compute CLI tool ships on `rt-core` — a fraction of the full toolchain:

| Tier | Includes | Size | vs. toolchain |
| --- | --- | --- | --- |
| **rt-core** | VM, GC, core language, workers, caps, shared heap, zstd-bundle | **5.75 MB** | 13.3% |
| **rt-local** | + json/msgpack, files/process/env, sqlite, dates, crypto, compress, terminal, logging | **13.9 MB** | 32.0% |
| **rt-net** | + HTTP(S)/WS/TCP/UDP + servers, Postgres, Redis, telemetry | **20.4 MB** | 47.1% |
| **rt-full** | + intl, AI, FFI (everything runtime-shaped) | **32.6 MB** | 75.3% |
| _full toolchain_ | _(today's `--native` stub: VM + the whole compiler/LSP/DAP/formatter/REPL)_ | _43.3 MB_ | 100% |

(Measured on an arm64 release build; re-run `bench/rt_size_matrix.sh` for your platform.) Force
a tier with `--tier`; build a stub with the program's *exact* feature set via `--exact`.

## Cross builds (`--target`)

The embedded payload is **platform-independent** — it carries bytecode, constants, and
logical module keys, with no machine word-size, endianness, or path dependence. A cross build
is therefore a plain append of the same payload onto a per-target stub: `--target <triple>`
(one of the 8 published triples) resolves a stub for that triple through the ladder (a cross
target has no sibling/`current_exe` fallback, so it needs `--stub` or a fetched stub) and
appends. The output gets a `.exe` extension for a `*-windows-*` target regardless of host.

On macOS the **sign-before-append** rule does the work for cross builds: prebuilt darwin
stubs are ad-hoc signed once at release time, and because the signature's `codeLimit` covers
only the clean stub's bytes, appending the payload never invalidates it — so cross-building to
macOS needs no signing machinery on the build host. (The builder only ad-hoc signs locally for
the `current_exe` rung, where the running mac binary's own signature would otherwise break.)

## CLI flags

| Flag | Effect |
| --- | --- |
| `--native` | Produce a self-contained native executable instead of a `.aso`. |
| `-o, --out <path>` | Output path (defaults to `<stem>.aso`, or `<stem>` with `--native`). |
| `--strip` | Omit the optional debug section (source + line/variable tables). A stripped bundle degrades to span-only panic messages (no source frame). |
| `--target <triple>` | Cross-build for one of the 8 published triples (an unknown triple is rejected with the supported set). `--target <host>` ≡ omitting it. |
| `--stub <path>` | Append onto an explicit local `ascript-rt` stub (rung 1). |
| `--no-fetch` | Skip the network fetch rung (availability fall-through, never an integrity bypass). |
| `--tier <rt-core\|rt-local\|rt-net\|rt-full>` | Force the stub tier instead of automatic nearest-superset selection. |
| `--exact` | Build a stub with the program's *exact* feature set via local cargo (needs `cargo` + an AScript source checkout at `$ASCRIPT_SRC` matching this toolchain's version). The result is content-addressed and reused. |
| `--compress` | zstd-compress the embedded payload (the stub decompresses at startup). |
| `--oci` / `--oci-tag <tag>` | Write a loadable OCI image tarball instead of a bare executable (see below). |
| `--report-json <path\|->` | Emit the canonical JSON build report. |
| `--deny <caps>` | Embed a denial of one or more caps (comma-separated/repeatable). |
| `--sandbox` | Embed a denial of all five caps (`fs,net,process,ffi,env`). |
| `--deny-net <mode>` | Net carve-out: `external` (block public, allow loopback/private) or `all`. |
| `--deny-fs <mode>` | Fs carve-out: `write` (reads allowed) or `all`. |

`ASCRIPT_DENY` (an environment variable, not a flag) subtracts further at launch — see above.

## Container images (`--oci`)

`ascript build --oci app.as -o app.tar` writes a **loadable OCI image tarball** — no Docker (or
any container runtime) needed at build time. The image is `scratch`-based (no base layers), so the
bundle must be statically linked: `--oci` builds for a `*-unknown-linux-musl` target (defaulting to
`<host-arch>-unknown-linux-musl`; a gnu/darwin/windows triple is rejected with the musl equivalent
named). The single layer holds `/app` = the bundle, with `Entrypoint: ["/app"]`. The tag defaults to
`<stem>:latest` (override with `--oci-tag`). `--oci` composes with `--compress`, `--target`, and
`--tier`.

The output is **reproducible**: same source + same stub + same flags ⇒ byte-identical tarball
(fixed timestamps — `1970-01-01T00:00:00Z`, or `SOURCE_DATE_EPOCH` when set; sorted entries; pinned
gzip).

```text
ascript build --oci app.as -o app.tar
podman load -i app.tar && podman run --rm app:latest
```

> [!NOTE] The tarball is a standard **OCI image layout**, which `podman load`, `skopeo`, `buildah`,
> `nerdctl`, and Docker's containerd-snapshotter store all accept directly. Docker's **classic
> (overlay2) image store** rejects OCI-layout archives ("does not contain a manifest.json"); use
> `podman load`, or `skopeo copy oci-archive:app.tar docker-daemon:app:latest`, or enable the
> containerd image store, to load into such a daemon.

---

See also: [Modules & async](modules-async) for `import`/`export`,
[Capabilities & sandboxing](../stdlib/caps) for the cap model, and
[Compilation & runtime](../runtime) for the `.aso`/VM picture.
