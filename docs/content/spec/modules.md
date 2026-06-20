# Modules, imports & packages

This chapter specifies how AScript programs are split across files, how `import`
and `export` connect them, how a module specifier is classified, and what is
normative about package resolution. The syntactic forms are in the
[grammar chapter](grammar); this chapter gives their meaning. Capability gating of
stdlib *calls* (not imports) is in the capabilities chapter.

## Modules

**One file is one module.** A module's top-level declarations form its scope; a
module exports a subset of its bindings and imports bindings from other modules by
specifier.

A module is **evaluated once** and its result **cached**: importing the same
specifier from several places runs the module body a single time and shares the
same bindings. Evaluation is **eager** — importing a module runs its top level to
completion before the importing module continues (subject to the circular-import
rule below).

```as
// geometry.as
export const PI = 3.14159
export fn circleArea(r) { return PI * r * r }
export class Rect {
  fn init(w, h) { self.w = w; self.h = h }
  fn area() { return self.w * self.h }
}
```

### Exports

A top-level declaration MAY be marked `export`, which makes its binding visible to
importers. Any declaration kind may be exported: `export const`, `export let`,
`export fn`, `export class`, `export enum`, `export interface`. A non-exported
top-level binding is module-private.

AScript has **no default export**. There is no `export default` form and no
`import name from "…"` form; an import statement MUST use named imports
(`import { a } from …`) or a namespace import (`import * as ns from …`). Writing
`import f from "./mod"` is a **parse error** (`expected '{' or '*' after import`).

### Imports

Two import forms bring bindings into the importing module:

- **Named import** — `import { a, b as c } from "spec"` binds the exported names
  `a` and `b` (the latter under the local alias `c`). Importing a name the target
  module does not export is an error.
- **Namespace import** — `import * as ns from "spec"` binds a single namespace
  object `ns` whose members are the target module's exports (`ns.a`, `ns.b`).

```as
import { circleArea, Rect } from "./geometry"
import * as geo from "./geometry"

print(circleArea(2))    // 12.56636
let r = Rect(3, 4)
print(r.area())         // 12
print(geo.PI)           // 3.14159
```

An imported binding is a **snapshot of the exported value at import time**. For the
common cases — functions, classes, and `const` bindings, which are themselves
immutable — this is indistinguishable from a live binding. A program MUST NOT rely
on observing a later reassignment of an exported mutable `let` through a previously
imported name; the import binding is the value captured when the import resolved.

### Circular imports

If module *A* imports module *B* while *B* (directly or transitively) imports *A*,
the second import resolves to the **partially-initialized** module: the bindings
already evaluated are visible, those not yet evaluated are not. Referencing a
not-yet-initialized binding from a circular import during module load is a **load
error**, not silent `nil`. Code that runs *after* both modules finish loading
(e.g. inside a function called later) sees the fully-initialized bindings, so
mutual recursion across modules via function bodies is well-defined.

## Module-scope globals & late binding

A **direct top-level declaration** — a `let`, `const`, `fn`, `class`, `enum`,
`interface`, or `import` at the outermost level of a module — is a **module-scope
global**, not a frame-local. Module globals are **late-bound**: a function body or
a field default MAY reference a binding declared **later** in the same module,
because the reference is resolved when the code *runs*, not where it is written.

```as
fn a() { return b() }   // forward reference to b, declared below
fn b() { return 7 }
print(a())              // 7
```

This late-binding rule is the same one that lets the REPL persist definitions
across lines and lets the two engines (tree-walker and VM) agree on
forward-reference behavior. It does **not** extend into nested scopes: a `let`
inside a function is an ordinary lexically-scoped local.

## Specifier classification

The string after `from` is a **module specifier**. Its first characters classify
which loader resolves it:

- **`std/…`** — a **built-in standard-library module**. `import { abs } from
  "std/math"` resolves to native bindings; no file is read. The set of `std/*`
  modules present depends on the build's feature flags (see [stdlib](../stdlib/overview)).
- **`./…`, `../…`, or an absolute path** — a **relative file module**, resolved
  against the importing file's directory (or the filesystem root). The `.as`
  extension is implied.
- **A bare specifier** (e.g. `json5`, `acme/widgets`) — a **package**. The first
  path segment names the package; any remaining segments are a subpath within it.
- **`host:…`** — a **host module**, provided by an embedding host. Host modules
  exist only when the runtime is embedded; cross-link [embedding](../embedding).

A bare specifier that does not resolve to a known package is a clean error:

```
unknown package 'nosuchpkg' — add it with 'ascript add'
```

This is a load-time error with a source-pointed diagnostic, never a silent
fallback to a relative path.

## Package resolution

Packages are declared in the project manifest `ascript.toml` and resolved by the
built-in package manager. The following is normative for how a dependency graph is
turned into an on-disk module set.

### Manifest dependency shapes

`[dependencies]` entries select a **source kind** by value shape:

- `{ git = "url", tag = "…" }` or `{ git = "url", rev = "…" }` — a git source at a
  tag or revision.
- `{ url = "…" }` — a fetched archive source.
- `{ path = "…" }` — a local-path source.
- A bare version string (e.g. `"1.2"`) selects the **registry** source. The
  registry source kind is **RESERVED**: today it is a clean error, and it is the
  designated future-additive source. A manifest MUST NOT depend on registry
  resolution succeeding in this version.

### Version selection & the store

When multiple dependencies request different versions of the same package,
resolution uses **Minimum Version Selection (MVS)**: the chosen version is the
**lowest** version that satisfies every requirement, making resolution
deterministic and reproducible without a separate solver.

Each resolved package is staged into a **content-addressed store** keyed by an
`asum1` digest — a SHA-256 over a normalized tree of the package's contents. Two
sources that produce byte-identical trees share one store entry.

### The lockfile

Resolution writes `ascript.lock`, recording the exact resolved version and `asum1`
of every dependency. `ascript run`/`test`/`install` update the lock as needed.

The `--locked` flag makes resolution **offline and fail-closed**: dependencies are
taken **exactly** from `ascript.lock`, re-hashed against the store, and **any**
drift — a missing lock entry, a changed version, or an integrity mismatch — is a
hard error. `--locked` performs no network access and is the mode for CI and
sandboxes.

### The CLI surface

The package manager is driven by `ascript add` / `remove` / `install` / `update` /
`lock` / `tree` / `verify`. `ascript add <pkg>` records a dependency in the
manifest and resolves it; `ascript install` materializes the manifest's
dependencies into the store and lock. The full CLI is in the
[CLI reference](../cli) and the [packages guide](../packages).

## Capability note

Importing a `std/*` module is **not** capability-gated — the import binds the
module's functions but performs no privileged action. The **calls** into
OS-touching functions are what the capability model gates, at a single stdlib
chokepoint. A program may freely `import * as fs from "std/fs"` under any
capability set; only an actual `fs.read(…)` consults the granted capabilities. See
the capabilities chapter.

## Conformance

The module and package semantics in this chapter are exercised by:

- `tests/modules.rs` — named and namespace imports, relative-file resolution,
  evaluate-once caching, and the frozen-value worker-airlock crossing across
  modules.
- `tests/pkg.rs` — manifest dependency shapes, MVS resolution, the
  content-addressed store, lockfile writing, and `--locked` fail-closed
  re-hashing (hermetic; no network).
- `examples/modules/main.as` + `examples/modules/geometry.as` — a two-module
  program using both a named import (`{ circleArea, Rect }`) and a namespace import
  (`* as geo`).

Run the example with `target/release/ascript run examples/modules/main.as`; it
prints `12.56636`, `12`, and `3.14159`. The unknown-package error is reproduced
directly: importing a bare specifier with no package entry prints
`unknown package 'nosuchpkg' — add it with 'ascript add'`.
