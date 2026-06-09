# `std/caps` — opt-out capabilities & per-isolate sandboxing

AScript is **batteries-included, opt-out not opt-in**: every dangerous capability is
**granted by default**, and you **subtract** the ones you don't want. This is the
inverse of the Deno model — you never have to remember `--allow-read` to read a file —
and it is paired with a **real memory boundary** (the worker isolate) so the
subtraction is genuinely enforceable, not API theater.

`std/caps` is **core** (no Cargo feature) — capabilities exist even in a bare
`--no-default-features` build.

## The five capabilities

A small, fixed, closed set — one per dangerous resource class:

| Capability | Governs |
|---|---|
| `fs` | filesystem read/write/metadata/listing; `io` stdin reads; `os` file ops |
| `net` | sockets, HTTP, **DNS** (`net.lookup`), WebSocket, UDP, servers, net-topology |
| `process` | spawning subprocesses |
| `ffi` | `ffi.open` (and therefore all native calls) |
| `env` | reading/writing environment variables |

The gate is applied **once, centrally**, at the single stdlib dispatch site, keyed by
module string — so DNS (`net.lookup`, which is *not* a connect site), stdin reads
(`io`), and host-topology leaks (`os.networkInterfaces`/`localIp`/`hostname`) are all
captured **by construction**. There is no per-function path that can slip the gate. A
denied capability is a **recoverable Tier-2 panic** (`capability 'net' denied`), so a
host can sandbox a plugin and *observe* the denial rather than crash.

## Three scopes, all subtractive

All three **subtract** from the default-all-granted set and **compose** (union of
denials — you can never re-grant via a later scope).

**1. CLI flags:**

```
ascript run app.as --deny ffi,process     # deny specific capabilities
ascript run app.as --deny-net=external    # granular: deny external net, allow loopback
ascript run app.as --deny-fs=write        # deny writes, reads still allowed
ascript run app.as --sandbox              # deny ALL five dangerous capabilities
```

**2. Manifest** `[capabilities]` in `ascript.toml`:

```toml
[capabilities]
deny = ["ffi", "process"]
net  = { deny = "external", allow = ["127.0.0.1", "api.internal"] }
fs   = { deny = "write", allow = ["./cache", "/tmp"] }
```

**3. In-code `caps.drop`** — an **irreversible, one-way** subtraction:

```ascript
import * as caps from "std/caps"

caps.has("ffi")        // -> bool          (query; never grants)
caps.list()            // -> array<string> (currently-granted capabilities)
caps.drop("process")   // irreversible subtraction for this isolate
caps.dropAll()         // drop every dangerous capability
```

There is **no `caps.grant`** — that is the whole point. Once dropped, a capability is
gone for the life of the (dedicated / top-level) isolate, so a program can drop
dangerous capabilities *before* dispatching untrusted code and trust the drop holds
(the same one-way narrowing that makes OpenBSD `pledge` / Linux `seccomp` trustworthy).

## Granular fs/net carve-outs

`fs` and `net` support "deny the class, allow a carve-out":

- **fs** — `deny = "write"` (reads still allowed) or `deny = "all"` with an `allow`
  list of path prefixes (canonicalized, so `./cache/../x` can't escape an allowed
  `./cache`).
- **net** — `deny = "external"` allows loopback/private addresses but blocks public
  ones; `allow` carves specific hosts back. Enforced at connect/bind with the resolved
  address.

These are the only granularity in the model — deliberately small for comprehensibility.

## The keystone — sandbox a plugin in a dedicated isolate

Capabilities are **per-isolate**, and a worker isolate has its own `Interp` + heap. So
you can run untrusted code in a **dedicated, memory-isolated** isolate carrying a
**reduced** capability set:

```ascript
import * as caps from "std/caps"
import * as ffi from "std/ffi"

worker fn plugin(input: number): string {
  let probe = recover(() => ffi.open("libm.so.6"))
  if (probe[1] != nil) { return `ffi denied — ${probe[1].message}` }
  return "ffi allowed"
}

// run the plugin in a DEDICATED isolate with ffi + process denied
let result = await run_in_worker(plugin, 0, { caps: { deny: ["ffi", "process"] } })
print(result)                       // ffi denied — capability 'ffi' denied
print(caps.has("ffi"))              // true — the HOST isolate is unaffected
```

Why this is a *real* security boundary and not an in-process API gate:

- **Memory isolation.** The worker has its own heap, `Interp`, and GC. The plugin
  cannot reach the host's objects by reference — only structured-clone bytes cross the
  airlock. An in-process gate can be bypassed by pointer tricks; a separate heap cannot.
- **Monotone, single-tenant drop.** A dedicated `run_in_worker({caps})` isolate is
  used by exactly one job, so an in-plugin `caps.drop` is terminal.

> **Pooled vs dedicated.** A cap-reduced job runs on a **dedicated** isolate, never the
> shared `worker fn` pool. The pool reuses one `Interp` across requests, so a durable
> `caps.drop` there would leak into the next request — `caps.drop` inside a pooled
> `worker fn` is therefore **refused** (a recoverable panic). A pooled worker simply
> inherits its dispatcher's capabilities as a read-only floor, re-established fresh per
> request. Durable, irreversible dropping happens only at the top level or in a
> dedicated isolate.

See also: [FFI (C interop)](ffi), [Workers & parallelism](../language/workers).
