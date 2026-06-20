# The capability model

This chapter specifies AScript's capability system: the unit of OS authority a
program holds, how authority is **subtracted**, where the runtime **enforces** it,
and the inversion that applies when AScript is **embedded**. Capabilities gate
**calls** into OS-touching stdlib functions, not imports (see the
[modules chapter](modules)); they ride the per-isolate model of the
[concurrency chapter](concurrency).

## The model

Capabilities are **opt-OUT**. Every capability is **granted by default**, so a
program that never subtracts a capability runs **identically** to a
capability-unaware runtime — existing programs are byte-for-byte unaffected. There
is **no grant operation**: authority only ever moves in the subtractive direction.
A capability, once removed for a scope, cannot be added back within that scope.

> The embedding host inverts this default — see [Embedding inversion](#embedding-inversion).

## The five capabilities

There are exactly five capabilities. Each names a class of OS authority; the
runtime maps each privileged stdlib path to the capability it requires.

| Capability | Governs |
|------------|---------|
| `fs`       | Filesystem reads/writes, directory listing, stat, path I/O. |
| `net`      | Sockets, HTTP, **DNS lookup**, WebSocket, UDP, servers, and network-topology queries (`os.networkInterfaces`, …). |
| `process`  | Spawning subprocesses. |
| `ffi`      | `ffi.open` and therefore every native foreign call. |
| `env`      | Environment variables and host/OS metadata. |

Coverage is **by construction**: because the gate keys on the *required*
capability of each path, DNS resolution (`net.lookup`, even though it is not itself
a connect) requires `net`, standard-input reads require the io/`fs`-era gate, and
OS-topology queries require `env`/`process` per their mapping. There is no
ungated back door to an OS resource.

A capability requirement is a **conjunction** in general: a single function MAY
require **more than one** capability, and the gate is satisfied only if **all** of
them are granted. For example, `std/docker` operations require `net` **and**
`process` together; if either is denied the call is refused (the first denied
capability, in canonical order, names the error). A function with a single-cap
requirement is the common case and behaves exactly as a one-capability check.

## Subtraction scopes

Capabilities are removed at three scopes, all **monotone** (a later scope can only
remove more, never restore):

1. **CLI flags** on a run:
   - `--deny <cap>` — deny one or more capabilities (comma-separated / repeatable),
     e.g. `--deny fs,net`.
   - `--sandbox` — deny **all five** (`fs,net,process,ffi,env`).
   - `--deny-net=external|all` and `--deny-fs=write|all` — **granular carve-outs**
     within `net`/`fs` (e.g. allow loopback but block public hosts; allow reads but
     deny writes).
   For `build` / `build --native`, the composed denial set is additionally
   **embedded in the produced artifact** and enforced at launch (further
   restrictable, never re-grantable, via `ASCRIPT_DENY`).
2. **Manifest** — the `ascript.toml` `[capabilities]` table denies capabilities for
   every run of the project.
3. **In-code** — `caps.drop("<cap>")` (and `caps.dropAll()`) subtracts a capability
   at runtime. This is **IRREVERSIBLE**: there is no `caps.grant`, and a dropped
   capability stays dropped for the rest of the isolate's life. `caps.has("<cap>")`
   queries the current set.

The effective set for a call is the default-all-granted set minus every scope's
denials.

## Enforcement points

Enforcement is concentrated, not scattered:

- **The single stdlib chokepoint.** Immediately before a `std/*` call is
  dispatched, the runtime consults the function's required capability (a possibly
  multi-capability requirement) against the granted set. A denied call is **never
  executed**. The gate fires **before** the OS is touched — a denied `fs.read`
  never opens a file descriptor.
- **The per-open-handle re-check.** Holding an already-open native handle (a file,
  socket, FFI library) does **not** outlive a later `caps.drop`: a method call on
  such a handle re-checks the handle's governing capability against the current set.
  An already-open handle **does not survive** a drop of its governing capability.

When nothing has been dropped (the default), the gate short-circuits on a single
"all granted" check — the default path carries **no per-call cost**.

## Denial behavior

A denied call raises a **recoverable Tier-2 panic** of the exact form:

```
capability '<cap>' denied
```

where `<cap>` is the lowercase capability name (`fs`, `net`, `process`, `ffi`,
`env`). It is a clean, source-pointed panic that a host MAY `recover` — never a
silent `nil`, a partial effect, or a process abort.

```as
import * as fs from "std/fs"
let [v, e] = fs.read("/etc/hosts")     // under --deny fs:
// raises: capability 'fs' denied
```

For a multi-capability requirement, the **first denied** capability in canonical
order names the error.

## Workers

Capabilities are **per-isolate** (per the [concurrency chapter](concurrency)): each
worker isolate carries its own capability set.

- **`run_in_worker(fn, input, { caps: { deny: [...] } })`** spawns a **dedicated**
  isolate with a **reduced** capability set and runs `fn` there. Because the isolate
  shares no memory with the host, this is a **real, memory-isolated sandbox** — not
  an in-process API gate. The host keeps its own capabilities; only the dedicated
  isolate is restricted. This is the supported way to run untrusted code under a
  narrowed capability set.

  ```as
  worker fn plugin(input: number): string {
    let probe = recover(() => ffi.open("libm.so.6"))
    return probe[1] != nil ? `denied: ${probe[1].message}` : "allowed"
  }
  // the plugin isolate has ffi + process denied; the host is unaffected:
  let result = await run_in_worker(plugin, 0, { caps: { deny: ["ffi", "process"] } })
  ```

- **`caps.drop` inside a pooled `worker fn` is REFUSED.** A pooled isolate is reused
  across requests, so an in-code drop would leak into a later, unrelated request.
  Attempting it is a loud recoverable panic directing the program to drop in a
  dedicated isolate (`run_in_worker`) or at the top level instead. Subtract a
  pooled worker's capabilities at the point that **creates** it, not inside its
  body.

## Embedding inversion

When AScript is **embedded** in a host program (the Rust/C API), the default
inverts: the host constructs each isolate with **`deny_all`** — `fs`, `net`,
`process`, `ffi`, and `env` all **denied** — and explicitly chooses what to allow.
The rationale is ownership of the blast radius: a CLI program is the artifact its
own author shipped (so all-granted is the convenient default), whereas embedded
script runs inside a *host's* process, and the host — not the script's author —
owns what it may touch. The subtractive, no-grant runtime model is unchanged; only
the **starting** set differs. See the [embedding guide](../embedding) for the host
API.

## Conformance

The capability model in this chapter is exercised by:

- `tests/cap_audit.rs` — the end-to-end denial audit (Gate 10): under `--sandbox`,
  per-capability `--deny`, and in-code `caps.drop`, **every** OS-touching stdlib
  path raises `capability '<cap>' denied`, and a granted capability still works;
  it also pins the pooled-`worker fn` `caps.drop` refusal.
- `examples/caps_sandbox.as` — `run_in_worker` with `{ caps: { deny: [...] } }`
  sandboxing a plugin isolate while the host keeps its capabilities.

Run the example with `target/release/ascript run examples/caps_sandbox.as`; it
prints `host has ffi: true`, `plugin: ffi denied — capability 'ffi' denied`, and
`host still has ffi: true`, demonstrating per-isolate subtraction. A CLI denial is
reproduced directly: under `--deny fs`, an `fs.read` raises `capability 'fs'
denied` before any file descriptor is opened.
