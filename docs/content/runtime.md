:::eyebrow Introduction

# Compilation & runtime

AScript programs are compiled to bytecode and executed by a **virtual machine**. This page explains
the engine model, the `ascript build` / `.aso` workflow, the garbage collector, the performance
characteristics, and the `--tree-walker` escape hatch.

## The engine model

`ascript run program.as` does two things in one step: it compiles your source to bytecode (lexer →
CST front-end → resolver → bytecode compiler → a `Chunk`), then runs that bytecode on the **bytecode
VM**. The VM is the default and only production engine.

A second engine — the legacy **tree-walker** — is retained, but only as a development aid. It serves
as a differential *oracle* (the test suite checks that the VM and the tree-walker produce
byte-identical output across the whole example corpus) and as a debugging escape hatch. You will
almost never need it.

> [!NOTE] The choice of engine never changes what a program *means*. The VM and the tree-walker are
> verified to be behaviorally identical; the only difference is speed.

### Why a VM

The bytecode VM brings three concrete benefits over walking the syntax tree directly:

- **Heap-allocated call frames.** Recursion depth is bounded by available heap, not by the native
  call stack — deeply recursive programs that would overflow a tree-walker run fine.
- **Adaptive specialization.** The VM warms up *inline caches* for property and method access and
  *adaptive arithmetic* for hot numeric operations, so frequently-executed code gets faster as it
  runs.
- **An ahead-of-time compilation artifact.** Because compilation produces a self-contained bytecode
  `Chunk`, that chunk can be serialized to disk and run later with no compile step — see `.aso`
  below.

## `ascript build` and `.aso`

`ascript build` compiles a `.as` program to a `.aso` bytecode file:

```text
ascript build app.as              # → app.aso
ascript build app.as -o out.aso   # choose the output path with --out / -o
```

Run the artifact directly — there is no compilation step, the VM loads and executes the bytecode:

```text
ascript run app.aso
```

Think of `.aso` as a **compilation cache and distributable artifact**. It is handy when you want to
ship a compiled program or skip recompilation on every run.

> [!WARN] An `.aso` file is **not a stable cross-version format.** It carries a magic header and a
> format version, and the runtime verifies both on load. A corrupt file, or one produced by a
> different `ascript` build whose bytecode layout has changed, is rejected with a clear error rather
> than executed:
>
> ```text
> error: cannot load app.aso: .aso format version mismatch: file is v5, this build expects v6 (recompile from source)
> ```
>
> Treat `.aso` as a cache keyed to your current `ascript` binary: rebuild it from source after
> upgrading.

## Garbage collection

AScript manages memory with a **cycle-collecting garbage collector** (reference counting plus a
Bacon–Rajan trial-deletion cycle collector):

- **Acyclic data is freed immediately and deterministically** when its last reference goes away —
  the common case pays no collector overhead.
- **Reference cycles are reclaimed by periodic collection.** A structure that points back at itself
  (`let a = []; a.push(a)`) or a web of mutually-referencing objects/closures cannot be freed by
  reference counting alone, so the cycle collector reclaims it — at program end, and during
  long-running work such as a `http.serve` loop, keeping memory bounded.
- **Native OS resources are not on the GC graph.** Files, sockets, and child processes are released
  by deterministic `Drop` the moment they go out of scope — never deferred to a collection cycle.
  Resource cleanup stays predictable regardless of what the GC is doing.

## Performance

On compute-bound code the bytecode VM is roughly **2–3× faster** than the tree-walker (geometric mean
~2.5× across the repository's `std/bench` benchmark suite — deep recursion, tight numeric loops,
property read/write, and method dispatch). Allocation-bound workloads (e.g. heavy string building)
see a smaller margin, because both engines pay the same allocator cost. These are machine-dependent
figures from a release build; re-run the bench suite to measure your own hardware.

## The `--tree-walker` escape hatch

To run a program on the legacy tree-walker instead of the VM:

```text
ascript run file.as --tree-walker          # flag must precede the file
ASCRIPT_ENGINE=tree-walker ascript run file.as   # env var equivalent
ascript repl --tree-walker                 # the REPL on the tree-walker
```

The `--tree-walker` flag takes precedence over the `ASCRIPT_ENGINE` environment variable. It applies
only to `.as` source — a `.aso` file is always run on the VM.

> [!WARN] The tree-walker is the **reference (legacy) front-end** and a debugging/differential aid,
> not a second dialect of the language. It expects canonical syntax (for example, a parenthesized
> condition: `if (cond) { … }`). Author your programs against the default VM engine; reach for
> `--tree-walker` only when you need to compare the two engines while diagnosing an issue.
