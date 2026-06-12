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

## CLI flags

| Flag | Effect |
| --- | --- |
| `--native` | Produce a self-contained native executable instead of a `.aso`. |
| `-o, --out <path>` | Output path (defaults to `<stem>.aso`, or `<stem>` with `--native`). |
| `--strip` | Omit the optional debug section (source + line/variable tables). |
| `--deny <caps>` | Embed a denial of one or more caps (comma-separated/repeatable). |
| `--sandbox` | Embed a denial of all five caps (`fs,net,process,ffi,env`). |
| `--deny-net <mode>` | Net carve-out: `external` (block public, allow loopback/private) or `all`. |
| `--deny-fs <mode>` | Fs carve-out: `write` (reads allowed) or `all`. |

`ASCRIPT_DENY` (an environment variable, not a flag) subtracts further at launch — see above.

---

See also: [Modules & async](modules-async) for `import`/`export`,
[Capabilities & sandboxing](../stdlib/caps) for the cap model, and
[Compilation & runtime](../runtime) for the `.aso`/VM picture.
