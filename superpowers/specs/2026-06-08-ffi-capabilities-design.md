# AScript FFI & Opt-Out Capability Permissions — Design (FFI)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** FFI (System-access tier of the Serious Language campaign — see `goal.md`)
- **Depends on:** **NUM** (real `int` = i64; pointer-width and sized-int marshalling are expressed
  *over* `int` — there is no FFI without exact integers). Workers Spec A (the per-isolate `!Send`
  model + the structured-clone airlock — the capability keystone is *per-isolate*).
- **Depended on by:** any native-API integration; the self-hosting story (call into C where AScript
  is not yet fast enough); SRV/BIN indirectly (a sandboxed plugin tier).
- **Engines:** both (tree-walker oracle == VM, byte-identical) — the whole feature lives at the
  `Value`/`Interp`/stdlib layer both engines share.
- **Breaking:** no new surface that changes existing semantics. Capabilities are **default-all-granted**,
  so every existing program runs unchanged. FFI is an additive `std/ffi` module.

---

## 1. Summary & motivation

AScript can talk to the OS through a rich native stdlib (`fs`, `process`, `net`, `crypto`, …), but it
**cannot call an arbitrary C library** — there is no way to `dlopen("libsqlite3.so")` and invoke a
symbol the runtime was not compiled against. A "serious general-purpose language" that stands next to
Java (JNI/Panama), C# (P/Invoke), Swift (C interop), and Go (cgo) needs a first-class **foreign
function interface**: open a shared object, look up a symbol, marshal arguments across the C ABI, and
get the result back as an ordinary AScript value. This is the last missing primitive for the
"system access (native + safe)" tier, and it is what lets the ecosystem wrap any C library without a
Rust recompile.

FFI is, by nature, **unsafe**: a wrong signature or a bad pointer is memory-unsafe in a way nothing
else in AScript is. That danger is what makes the *second* subsystem in this spec non-optional and
co-designed: a **capability permission model**. AScript's pillar-2 stance (`goal.md`: "batteries
included by default, **opt-out not opt-in**") forbids the Deno model where you must remember
`--allow-read` to read a file. Instead **every capability is granted by default** and you **subtract**
the ones you don't want — at three scopes (CLI, manifest, in-code) — and the subtraction is
**enforced per-isolate**. The keystone insight that makes this a *real* security boundary rather than
API theater: capabilities ride the **share-nothing worker isolate** from Workers Spec A. Untrusted
code runs in a **dedicated** (single-tenant) worker spawned with a reduced `CapSet` — **not** the
shared pool, whose `Interp` is reused across requests and so cannot hold an irreversible drop (§4.5a);
because that dedicated isolate is **memory-isolated** (its own heap, its own `Interp`, only `Send`
bytes cross the airlock — `src/worker/isolate.rs:35`), denying it `ffi`/`process`/`net` is enforceable
in a way an in-process API gate never could be.

The two subsystems are designed together because **neither is safe alone**: FFI without capabilities
is an unguarded `dlopen`; capabilities without a memory boundary are bypassable. Together they give
"batteries fully included, but you can hand a plugin a locked-down isolate."

### Two design tenets locked up front

1. **Sized C ints are an FFI *marshalling* concern, not a `Value` kind.** `i8/i16/i32/u8/u16/u32/u64/
   size/ptr` exist **only at the C-ABI boundary**, described in AScript as `ffi.i32` etc. and carried
   in/out over the NUM `int` (i64). There is **no new `Value` variant** for sized integers (NUM §10
   already reserved this decision for FFI). This keeps `Value` cheap and the numeric tower
   un-fragmented; the boundary does the narrowing/widening with documented overflow behavior (§3.3).
2. **FFI is a DEFAULT-ON feature, not default-off.** Batteries-included means `import "std/ffi"` works
   out of the box. Safety comes from the **capability gate + the per-isolate sandbox**, *not* from
   compiling the module out (§3.6 justifies this against the obvious "default-off for safety"
   objection).

## 2. The two subsystems at a glance

| Subsystem | What it adds | Where it lives |
|---|---|---|
| **FFI** | `std/ffi`: open libs, look up symbols, call across the C ABI; C-type marshalling over `int`/`Bytes`/pointer handles | new `src/stdlib/ffi.rs`; `libloading` + `libffi` deps; `NativeKind::Foreign{Lib,Symbol,Ptr}` |
| **Capabilities** | a per-`Interp` `CapSet` (default = all granted); deny via CLI / manifest / irreversible in-code `caps.drop`; checked **centrally** at the single `&self` stdlib dispatch site (`call_stdlib`, §4.3) keyed by module string — covering **every** resource-acquiring module incl. DNS (`net.lookup`); **per-isolate** → the sandbox keystone (dedicated, not pooled, for cap-reduced work, §4.5a) | new `src/stdlib/caps.rs`; `Interp.caps`; `WorkerRequest`/dedicated-spawn carry a `CapSet`; `main.rs` flags; `[capabilities]` in `ascript.toml` |

## 3. The FFI model

### 3.1 Loading & symbols

`std/ffi` wraps **`libloading`** (cross-platform `dlopen`/`LoadLibrary`) for library + symbol
resolution and **`libffi`** (the `libffi` crate, a libffi-style trampoline) for the actual C calling
convention — so an arbitrary `(args) -> ret` signature can be invoked at runtime without compile-time
knowledge of the symbol.

```
import { ffi } from "std/ffi"

let [lib, err] = ffi.open("libm.so.6")     // [ForeignLib, err]  — Tier-1 Result
let cos = lib.symbol("cos", [ffi.f64], ffi.f64)   // ForeignSymbol, signature bound
print(cos.call([1.0]))                      // 0.5403...   (f64 in, f64 out)
```

- **`ffi.open(path) -> [lib, err]`** — `dlopen`. A failure (file not found, not a shared object,
  missing dependency) is a **Tier-1 `[value, err]`** (recoverable data, not a panic): you may probe
  for an optional library. The handle is a `NativeKind::ForeignLib` resource (§3.4).
- **`lib.symbol(name, argtypes, rettype) -> symbol`** — `dlsym` + a **bound signature**. A missing
  symbol is **Tier-1** too (`lib.symbol` returns `[symbol, err]`); a malformed signature
  (`argtypes` not an array of `ffi.*` type descriptors) is a **Tier-2 panic** (programmer misuse,
  the §3.3 marshalling-misuse rule). The handle is a `NativeKind::ForeignSymbol`.
- **`symbol.call(args) -> ret`** — marshal `args` per the bound `argtypes`, invoke through the
  libffi trampoline, marshal the result back per `rettype`. An **arity / marshalling mismatch** at
  call time (wrong arg count, an `int` that doesn't fit the declared `ffi.u8`, a non-`Bytes` where a
  `ffi.ptr` is required) is a **Tier-2 panic** with a clear message (it is a bug in the binding, not
  recoverable data).

The **dlopen/dlsym = Tier-1 vs marshalling-misuse = Tier-2** split is deliberate: *which library
exists on this machine* is a runtime/environment fact you legitimately handle; *calling a symbol with
the wrong shape* is a programming error in the same class as `array index must be an int`.

### 3.2 C types, described in AScript

The marshalling vocabulary is a set of **type-descriptor values** exported by `std/ffi` (tagged
Objects / small natives — **not** new `Value` kinds, exactly like `std/schema`'s tagged-Object
schemas). They describe a C type at the boundary:

| `ffi.*` descriptor | C type | AScript value in/out | Notes |
|---|---|---|---|
| `ffi.i8 i16 i32 i64` | `int8_t … int64_t` | `int` | NUM `int` (i64); narrower types range-checked (§3.3) |
| `ffi.u8 u16 u32 u64` | `uint8_t … uint64_t` | `int` | `u8/u16/u32` range-checked into i64; `u64` carries the i64 **bit pattern** (no sign check, §3.3) |
| `ffi.f32 ffi.f64` | `float`/`double` | `float` | NUM `float` (f64); `f32` narrows on the way out, widens on the way in |
| `ffi.size` | `size_t`/`ssize_t` | `int` | pointer-width; resolved per target at marshal time |
| `ffi.ptr` | `void*` / any `T*` | `Bytes` **or** `ForeignPtr` | a `Bytes` passes its buffer address; a `ForeignPtr` passes the opaque pointer |
| `ffi.cstr(s)` | `const char*` | constructed from `Str` | `ffi.cstr("hi")` → NUL-terminated `Bytes` you pass as a `ffi.ptr` |
| `ffi.void` | `void` | `nil` | return type only |

Because sized ints marshal over `int`, the **AScript side never sees an `i32` type** — it sees an
`int` and a *boundary descriptor* that says "this `int` is an `i32` here." This is the NUM §10
decision realized: the sized integer "lives only at the boundary."

### 3.3 Marshalling rules (the narrowing/widening contract)

- **Integer narrowing is checked, never silent (pillar 1) — for the SIGNED and small-unsigned types.**
  Passing an `int` that does not fit the declared C type is a **Tier-2 panic**
  (`ffi: value 300 out of range for u8`). Silent truncation is forbidden by `goal.md` gate 6
  ("silent numeric truncation … is a bug"). The range check applies as follows:
  - `ffi.i8/i16/i32/i64`: the `int` must be in the signed range of that width (`i64` is always in
    range). Negative values are fine.
  - `ffi.u8/u16/u32`: the `int` must be in `0 ..= TYPE_MAX` (e.g. `0..=255` for `u8`); negative or
    too-large → panic.
  - **`ffi.u64` (and `ffi.size` on a 64-bit target): NO sign range-check — the i64 bit pattern IS the
    value.** This is the carve-out that resolves the earlier contradiction. AScript `int` is a signed
    i64; the full u64 value space is therefore addressed by its **two's-complement bit pattern**. Passing
    `-1` to a `ffi.u64` marshals as `0xFFFF_FFFF_FFFF_FFFF` (all-ones), the standard way to express
    `u64::MAX` from a signed integer; passing any i64 is accepted and reinterpreted bit-for-bit as the
    u64. There is no "negative → u64 panics" rule (the prior draft's contradiction): for u64/`size`,
    the bits are the contract, exactly mirroring the **output** asymmetry below. (`u8/u16/u32` keep the
    range-check because their bit width is *narrower* than `int`, so an out-of-range value genuinely
    loses bits — unlike u64, which is the same width as `int`.)

  Widening (`i32` → AScript `int`) is exact and lossless.
- **`u64`/`size` round-trip is bit-pattern-symmetric.** On **input**, an i64's bits are taken as the u64
  value (above). On **output**, a `u64`/`size` return whose top bit is set comes back as the
  **two's-complement bit pattern** (a negative `int`); `math`/bit ops (NUM) recover the unsigned value.
  Documented, not silent — `type_name` stays `int` in both directions. A value that round-trips
  (`u64`-in then same `u64`-out) is bit-identical.
- **`f32` round-trips with precision loss** (it is a 32-bit float); documented in the descriptor.
- **Strings:** `ffi.cstr(s)` produces a **NUL-terminated `Bytes`**; you pass the `Bytes` as a
  `ffi.ptr`. A returned `const char*` is read back via `ffi.read_cstr(ptr)` (copies until NUL into a
  `Str`) — never auto-decoded, because lifetime is the caller's concern.
- **Out-params & structs:** modeled on **`Bytes` + a layout descriptor**. `ffi.struct([["x",
  ffi.i32], ["y", ffi.f64]])` returns a layout value with `.size`, `.alloc()` (a zeroed `Bytes` of
  the right size + C alignment), and field `get(buf, "x")` / `set(buf, "x", v)` accessors that apply
  the computed offset. You pass the `Bytes` as a `ffi.ptr` (out-param), then read fields back out.
  This keeps structs in **ordinary `Bytes`** (a sendable, GC-traced-as-leaf value) rather than a new
  Value kind.

### 3.4 Handles (the three `NativeKind`s)

Following the established native-resource pattern (`CLAUDE.md` "Native resource handles": OS
resources live in `Interp.resources` keyed by id, referenced from script by a cheap `Value::Native`
handle — `src/interp.rs:447`, `src/value.rs:357`), FFI adds three `NativeKind` variants
(`src/value.rs:366`):

- **`ForeignLib`** — an open library. Backs a `ResourceState::ForeignLib(libloading::Library)`; its
  `Drop` `dlclose`s deterministically (the existing resource-reclaim discipline). Method: `.symbol`.
- **`ForeignSymbol`** — a `dlsym`'d symbol + its bound signature (`argtypes`, `rettype`) +
  a libffi CIF. Method: `.call`. **Storage:** it stores the resolved symbol as a **raw `*mut c_void`**
  (the function address, obtained once via `libloading::Library::get` then `Symbol::into_raw` / cast)
  **and keeps the owning `Library` alive** (an `Rc`/clone of the `ResourceState::ForeignLib`'s
  `libloading::Library`). It does **NOT** store a borrowed `libloading::Symbol<'lib>` — that type is
  tied to the `Library`'s lifetime `'lib` and **cannot be `'static`**, so it cannot live in a
  resource-table `ResourceState` (which is `'static`). The raw-pointer-plus-kept-alive-`Library`
  pairing is what gives both `'static` storage *and* lifetime correctness (the address stays valid
  because the `Library` is not dropped while any symbol referencing it is live). The `unsafe` deref of
  the raw pointer at call time is sound precisely because the `Library` is held.
- **`ForeignPtr`** — an **opaque C pointer** returned by a call (e.g. a `malloc` result, a
  handle from a C "constructor"). Carries the raw `usize` address. Passed back to other calls as a
  `ffi.ptr`. Read via `ffi.read_cstr`/struct accessors; freed by calling the library's own `free`
  symbol (AScript does **not** auto-free foreign pointers — ownership is the C library's contract).

**GC safety — the load-bearing invariant.** All three handles are `Rc`-backed `NativeObject`s and are
**untraced by the cycle collector**, exactly like every other native resource. `Value::trace`
(`src/gc.rs:176`) only recurses into the cycle-capable container variants (`Array/Object/Map/Set/
Instance/Closure`) and falls through `_ => {}` for `Native` — so `ForeignLib/Symbol/Ptr` are already
covered by the catch-all the moment the `NativeKind` variants are added. **They must never gain a
`Trace` edge:** a raw foreign pointer is opaque memory the GC cannot reason about, and the deterministic
`Drop` of `ForeignLib` is what reclaims the `dlopen` handle (`goal.md` gate 4: "native handles stay
GC-opaque (no `Trace`)"). A unit test asserts `Value::Native(ForeignPtr)` adds zero traced edges.

### 3.5 Threading caveat — FFI stalls the isolate

A C call goes through the libffi trampoline **synchronously on the calling isolate's single thread**.
AScript's runtime is `!Send` and cooperatively scheduled (`current_thread` tokio + `LocalSet`); there
is **no preemption**. Therefore **a blocking C call stalls the entire isolate** — no other task on
that isolate makes progress until the call returns. This is the FFI analog of the Workers Spec A
cost-model guidance and is documented as a first-class rule:

> **Offload slow or blocking FFI to a `worker fn`.** A long-running native call (image decode, a
> blocking I/O syscall in a C library) should run inside a worker isolate so the main isolate's event
> loop keeps serving. The result crosses back as ordinary structured-clone data (a `Bytes`/`int`/
> `Object` — *not* a `ForeignPtr`, which is isolate-local and non-sendable, §6).

`symbol.call` is therefore **synchronous** (returns the value directly, not a `future`). Wrapping it
in a `worker fn` is the explicit, documented path to non-blocking native work — composing the two
campaign subsystems exactly as intended.

> **Cancel/timeout cannot interrupt a live C call (documented limitation).** AScript's structured
> concurrency — `task` cancel-on-drop, `race`, `timeout` — works by **dropping the `await` future** so
> a parked task never resumes. A `symbol.call` is a **synchronous, non-`await` foreign call on the
> isolate thread**: once control enters the libffi trampoline, there is **no `.await` point and no
> preemption**, so a `timeout`/cancel that fires *during* the call **does not interrupt it** — it takes
> effect only after the C function returns and control comes back to AScript. A C call that blocks
> forever blocks that isolate forever (the §3.5 stall, taken to its limit). The only way to bound a
> hostile/slow native call in wall-clock time is to run it in a **dedicated worker isolate** and drop
> the *isolate* (terminating its OS thread) — not to `timeout` the inner call. This is an inherent
> property of FFI under a cooperative `!Send` runtime, stated here so it is not mistaken for a bug.

### 3.6 Why DEFAULT-ON (not default-off-for-safety)

The obvious objection: "FFI is memory-unsafe, so compile it out by default and make users opt in."
Rejected, because it conflates two orthogonal axes:

- **Availability** (is the module present in this binary?) — governed by the Cargo feature. FFI is a
  **default feature** so the batteries-included promise holds: `import "std/ffi"` just works.
- **Authority** (is this *isolate* allowed to use it?) — governed by the **capability gate**. The
  `ffi` capability is granted by default but **subtractable** per-isolate.

Default-off-availability would mean every FFI-using program needs a non-default build, which breaks
"batteries included" and gives only *illusory* safety (a default-off feature you turn on globally is
no safer than a default-on one you can deny per-isolate). The **capability model is the safety
mechanism**, and it is strictly more granular: you can grant FFI to the trusted main isolate and
deny it to an untrusted plugin worker in the *same* program — impossible with a compile-time switch.
FFI is gated behind a `ffi` **feature** only so a size-constrained `--no-default-features` build can
omit `libloading`/`libffi`; in the default build it is present and authority is the capability's job.

## 4. The capability model

### 4.1 The five capabilities

A small, fixed, **closed** set — one per dangerous resource class. Each capability governs a set of
stdlib *module strings* (the `module` arg of `call_stdlib`, `src/stdlib/mod.rs:276`), and the gate is
applied **once**, centrally, at that single `&self` dispatch site (§4.3) — NOT module-by-module:

| Capability | Governs | Gated module string(s) |
|---|---|---|
| `fs` | filesystem read/write/metadata/listing | `"fs"`; `"io"` (stdin reads, §4.3a); `"os"` file ops |
| `net` | sockets, HTTP, **DNS**, WebSocket, UDP | `"net"` (incl. `lookup`/`lookupOne` — DNS), `"net_tcp"`, `"net_http"`, `"net_udp"`, `"net_ws"`, `"http_server"` |
| `process` | spawning subprocesses | `"process"` |
| `ffi` | `ffi.open` (and therefore all native calls) | `"ffi"` |
| `env` | reading/writing environment variables | `"env"` |

> **[SECURITY — must enumerate, do not under-count.]** An earlier draft gated "~5 chokepoints" at the
> level of `tcp_connect`/`tcp_listen`/`call_process` etc. That is a **hole**: `net.lookup` /
> `net.lookupOne` (DNS resolution) route through `call_net` → `net_host.rs` (`src/stdlib/net_host.rs:51`,
> `Interp::call_net`), which is **NOT** a connect/bind site — so a per-connect gate leaves DNS *ungated*,
> and a sandboxed plugin under `--sandbox` / `caps.drop("net")` can still resolve arbitrary hostnames and
> exfiltrate data through the DNS query stream. The fix is to gate by **module string at the dispatch
> site** (§4.3), which captures `"net"` (and therefore `net.lookup`) by construction — there is no
> per-function path left ungated. The same reasoning forced auditing `io` and `os` below.

**`io` / `os` audit (resource-leaking entries that are NOT connect/bind/spawn):**

- **`io`** reads process **stdin** (`io.readAll`/`readLine`/`lines`, `src/stdlib/io.rs:32` `call_io`) —
  a real input channel an untrusted plugin should not be able to drain. **Decision: `"io"` is gated by
  `fs`** (it is a host-fd read, the closest existing class; a finer `io` capability is rejected as
  over-fragmentation, §9).
- **`os`** exposes `os.networkInterfaces()` / `os.localIp()` (`src/stdlib/os.rs:171,198`) and
  `os.hostname()` (`:88`) — these **leak network topology / host identity** without acquiring a socket.
  **Decision: the topology-leaking `os` entries (`networkInterfaces`, `localIp`) are gated by `net`**;
  the rest of `os` (pid, platform, arch, cpu-count, temp-dir, uptime, disks) is **not gated** — it is
  ambient host metadata that reveals nothing a process can't already see about itself, and
  gating it would break trivially-trusted introspection. `os.hostname` is borderline; it is gated by
  `net` alongside the topology calls (a hostname is an addressable identifier). This per-function split
  inside `os` is the one place the gate looks at `func`, not just `module` (§4.3a).

A denied capability is a **recoverable Tier-2 panic** (`capability 'ffi' denied`), catchable by
`recover` — so a host can sandbox a plugin and observe the denial rather than crashing. (Recoverable
because the host needs to handle a plugin that probes for a capability it doesn't have; it is Tier-2,
not Tier-1, because it is not data you thread with `?` — it's an authority violation.)

### 4.2 Opt-OUT, three scopes

All three scopes **subtract** from the default-all-granted set. They compose (union of denials).

**(1) CLI flags** (`src/main.rs`, the `Run` subcommand):

```
ascript run app.as --deny ffi,process        # deny specific capabilities
ascript run app.as --deny-net=external       # granular: deny external net, allow loopback
ascript run app.as --sandbox                 # deny ALL dangerous caps (fs,net,process,ffi,env)
```

`--deny` takes a comma list; `--sandbox` is sugar for denying the whole set; the granular
`--deny-net=external` / `--deny-fs=/etc` forms carry an allow-within-deny rule (§4.4).

**(2) Manifest** `[capabilities]` in `ascript.toml` (`src/pkg/manifest.rs` gains a third owned
table beside `[package]`/`[dependencies]`):

```toml
[capabilities]
deny = ["ffi", "process"]
# granular allow-within-deny (deny the class, carve back specific grants):
net  = { deny = "external", allow = ["127.0.0.1", "api.internal"] }
fs   = { deny = "write", allow = ["./cache", "/tmp"] }
```

Parsed entirely CLI-side (SP6 keeps TOML/IO out of the core), producing a `CapSet` handed to the
`Interp` at construction. Manifest denials and CLI denials **union** (you cannot re-grant via CLI what
the manifest denied — denial is monotone).

**(3) In-code `caps.drop`** — an **irreversible, one-way subtraction** for the current isolate:

```
import { caps } from "std/caps"

caps.drop("ffi")              // this isolate can never call ffi.open again
caps.dropAll()                // sandbox the rest of this isolate's run
if caps.has("net") { ... }    // query (returns bool; never grants)
```

`caps.drop` can **only subtract**. There is **no `caps.grant`** — that is the entire point: a
capability, once dropped, is gone for the **life of the dedicated isolate (or the top-level program)**,
so a program can drop dangerous capabilities *before* `eval`-ing or dispatching untrusted code and
trust that the drop holds. (This mirrors OpenBSD `pledge`/Linux seccomp's one-way narrowing — the
property that makes self-sandboxing trustworthy.)

> **Where `caps.drop` is durable.** It is irreversible on the **top-level program isolate** and on a
> **dedicated** (single-tenant) worker isolate spawned via `run_in_worker({caps})` (§4.5a). In a
> **pooled** `worker fn` — which REUSES one `Interp` across many requests (`isolate_loop`,
> `src/worker/isolate.rs:240`) — a drop would leak into the next, unrelated request, so `caps.drop`
> there is **refused** (a recoverable Tier-2 panic / no-op-with-warning, §4.5a). Self-sandboxing
> before handing off untrusted code therefore happens at the top level or via a dedicated isolate,
> never by mutating a shared pooled isolate.

### 4.3 Per-Interp `CapSet` + the ONE central dispatch-site gate

A `CapSet` is a small, `Send`, `Clone` value — a bitset of the five capabilities plus the optional
granular allow/deny lists (§4.4). It lives on `Interp` as `caps: RefCell<CapSet>` beside the existing
state cells (added to the `struct Interp` at `src/interp.rs:437`; `RefCell` only because `caps.drop`
mutates it — reads are cheap `Copy` snapshots of the bitset, **never held across `.await`**). Default
construction (`Interp::new`, `src/interp.rs:788`) yields **all-granted** — so every existing program and
every test is unaffected (the default path is byte-identical, the same discipline NUM/Workers used).

**The gate lives at ONE `&self` dispatch site, not "at the top of `fs::call`".** This is a correctness
requirement, not a style choice: `fs::call` (`src/stdlib/fs.rs:70`) and `env::call`
(`src/stdlib/env.rs:28`) are **free functions** with **no `&self`** — they cannot read `Interp.caps` at
all. The single place that has both `&self` *and* the `module` string for every stdlib call is
`Interp::call_stdlib` (`src/stdlib/mod.rs:276`), whose terminal `match module { … }`
(`src/stdlib/mod.rs:364`) dispatches each module. The cap check is inserted **immediately before that
match**, keyed off `module` (and, for `os`, off `func` per §4.3a):

```rust
// in call_stdlib, just before `match module { … }`:
if let Some(cap) = required_cap(module, func) {
    self.require_cap(cap, module, func, args, span)?;   // recoverable Tier-2 on denial
}
match module { … }
```

`required_cap(module, func) -> Option<Cap>` is the **complete, central enumeration** (§4.1): every
`"fs"`/`"io"`/`"os"`(file)→`Fs`, every `"net*"`/`"http_server"`/`os`-topology→`Net`,
`"process"`→`Process`, `"ffi"`→`Ffi`, `"env"`→`Env`. Because it is keyed at the dispatch root, **there is
no per-function path that can slip the gate** — adding a new `std/*` module forces a (possibly `None`)
entry here, and the §8 audit test asserts every resource-acquiring module is mapped (Gate 10).

For the **granular** carve-outs (§4.4) the address/path isn't known at the dispatch site, so those two
capabilities do a **two-stage** check: the dispatch-site stage tests the **bitset** (denied-outright →
panic now; granted-outright → pass; *granular-configured* → defer), and a second stage inside the net
connect/bind entries (`net_tcp::tcp_connect`/`tcp_listen`, `net_http` request, `net_udp`/`net_ws` binds)
and the fs path-resolving entries re-checks the resolved address/path against the allow-list. **Gate-12
fast path:** when **no** granular carve-out is configured for `fs`/`net` (the overwhelmingly common
case), the dispatch-site bitset test is *conclusive* and the second stage **short-circuits to the
bitset** with **no path canonicalization and no host comparison** — the all-granted / class-denied hot
path never canonicalizes (§4.4).

- **`ffi`** is gated at the `"ffi"` module string — a denied `ffi` blocks `ffi.open`, which transitively
  blocks all calls (you cannot get a symbol without a lib).

The check is a one-liner helper, `self.require_cap(Cap::Ffi, module, func, args, span)?`, that reads a
`Copy` snapshot of `caps` and, on a denied capability, returns the recoverable Tier-2 panic. Because the
gate sits at the one funnel every stdlib call already passes through, there is no behavior change when
all caps are granted — it is a cheap `Option` lookup + bitset test that passes.

#### 4.3a `os` per-function split

`os` is the lone module whose gating depends on `func`: `required_cap("os", "networkInterfaces")` and
`("os", "localIp")` / `("os", "hostname")` return `Some(Net)` (topology/identity leak,
`src/stdlib/os.rs:171,198,88`); every other `os` func returns `None` (ambient self-introspection — pid,
platform, arch, cpuCount, tempDir, uptime, disks). `io` returns `Some(Fs)` for its stdin readers
(`src/stdlib/io.rs:32`). `fs` returns `Some(Fs)` for all funcs; `env`/`process`/`net*` are whole-module.

### 4.4 Granular allow-within-deny (fs paths, net hosts)

`fs` and `net` support a **"deny the class, allow a carve-out"** refinement (the rest are
all-or-nothing). The `CapSet` carries, per those two capabilities, an optional `{ deny: Scope, allow:
Vec<Pattern> }`:

- **fs:** `deny = "write"` (reads still allowed) or `deny = "all"` with `allow = ["./cache"]`
  (only those subtrees). When a carve-out IS configured, the second-stage check canonicalizes the
  resolved path and tests prefix membership.
- **net:** `deny = "external"` allows loopback/private ranges but blocks public addresses;
  `allow = ["api.internal"]` carves specific hosts back. When a carve-out IS configured, the
  second-stage check runs at connect/bind with the resolved address.

**Gate-12 hot-path rule (load-bearing).** The granular path/host comparison is **only** reached when a
carve-out is configured for that capability. The `CapSet` carries `Option<FsScope>` / `Option<NetScope>`;
when it is `None` (the default, and the all-`--deny`/all-grant cases), the dispatch-site bitset test
(§4.3) is **conclusive** and the second-stage check **short-circuits to the bitset** — **no path
canonicalization, no host string comparison, no `to_addr` resolution** on the hot path. Canonicalization
is paid for *only* by programs that actually configure a carve-out. (This is why the carve-out lives in
an `Option`, not as always-empty allow/deny lists that would force a "is this path in the empty list?"
walk on every fs call.)

This is the only place granularity exists; it is deliberately small (paths + hosts) to keep the model
comprehensible. Anything finer is out of scope (§9).

### 4.5 THE KEYSTONE — capabilities are per-isolate → the sandbox

The decisive property: **a `CapSet` is per-`Interp`, and a worker isolate has its own `Interp`**
(`src/worker/isolate.rs:230` — `let interp = Rc::new(Interp::new())` is built *inside*
`run_isolate_thread`). So you can spawn a worker with a **reduced** `CapSet` and that isolate is a
**real sandbox**:

```
// run untrusted plugin code in a memory-isolated, capability-restricted DEDICATED isolate:
let [result, err] = await run_in_worker(plugin, input, {
  caps: { deny: ["ffi", "process", "net"] }   // fs+env only, in a separate heap
})
```

#### 4.5a [SECURITY] The pooled-isolate REUSE hazard — and why cap-reduced work must NOT use the pool

The naive design ("a `worker fn` installs its `CapSet` into the worker's `Interp` and `caps.drop`
holds for the life of the isolate") is **unsound against the shared pool**, and the original wording
was wrong. The verified reality (`src/worker/isolate.rs:240` `isolate_loop`): the pool builds **ONE**
`Interp`/`Vm` per isolate (`run_isolate_thread`, `:216–234`) and **reuses it across MANY requests** —
`while let Some(req) = rx.recv().await { … }` serves every dispatched `worker fn` on the **same**
`Interp`, and `pool.dispatch` round-robins requests onto whichever isolate is free
(`src/worker/pool.rs:68`). Two consequences make a per-request `CapSet` on a pooled isolate unsound:

1. **A `caps.drop` LEAKS forward.** If request A runs `caps.drop("net")`, the `CapSet` mutation persists
   on the shared `Interp` and request B (a *different*, possibly trusted `worker fn`) inherits the
   dropped state. Authority silently shrinks under an unrelated caller. (Verified: nothing in
   `isolate_loop` resets `Interp` state between requests; only `loaded`-slice tracking is per-loop.)
2. **Reinstalling a fuller `CapSet` per request IS a re-grant.** The only "fix" available on a reused
   `Interp` — write the request's `CapSet` into `caps` at the top of each request — **re-grants**
   capabilities a prior request dropped. That directly contradicts the one-way / monotone / never-re-grant
   property that is the *entire* security argument for `caps.drop` (§4.2). You cannot have both
   "irreversible drop" and "per-request CapSet on a shared `Interp`."

**Resolution (the security decision).** Capability-restricted / untrusted work does **NOT** run on the
shared pool. It runs on a **DEDICATED, single-tenant isolate** spawned for that one job:

- `run_in_worker(fn, input, opts)` with a `caps` option spawns a **fresh dedicated isolate** via
  `spawn_isolate` (`src/worker/isolate.rs:140` — the actor-style path, distinct from
  `Isolate::spawn`/`isolate_loop`). **Transport mechanism:** `spawn_isolate`'s inbound is a `Vec<u8>`
  byte channel, but its `make_loop` closure is `Send + 'static` (`isolate.rs:140–142`), so the `Send`
  `CapSet` is **captured directly in that closure** — it never needs to ride the byte channel and never
  touches the structured-clone value serializer (it is not a `Value`, §6). The closure installs
  `opts.caps` into the isolate's brand-new `Interp` **before** running any plugin code, runs the job,
  and tears the isolate down. The `Interp` is **never reused for another caller**, so `caps.drop` inside
  the plugin is irreversible *and* cannot leak — there is no "next request" on that `Interp`. This is the
  **actor-isolate model**, not the pool, and it is the recommended path the review called for.
- A **plain pooled `worker fn` does NOT carry a per-call `CapSet` and does NOT permit a leaking drop.**
  Pooled workers inherit the **dispatching isolate's caps as a frozen, read-only floor**: the
  `WorkerRequest` carries the caller's `CapSet` (§6), the isolate installs it **fresh at the top of
  each request** (so request B is unaffected by request A), and **`caps.drop` inside a pooled
  `worker fn` is a no-op-with-warning / refused** — dropping is only meaningful (and only durable) in a
  dedicated isolate or the top-level program. Because the per-request install always writes the
  caller-supplied floor (never reads back a prior request's mutated state), there is **no forward leak
  and no re-grant**: the install is idempotent re-establishment of the *caller's* authority, not a
  restoration of dropped authority (the pooled worker had no authority to drop in the first place).

> **Why this is consistent, not a re-grant loophole.** The monotone invariant is "**no isolate ever
> gains a capability it dropped.**" A pooled isolate never *drops* (drop is refused there), so writing
> the caller's floor at each request grants it nothing it ever had-and-lost. A dedicated isolate *can*
> drop, and is single-tenant, so its drop is terminal. The two regimes are disjoint and each is
> monotone on its own `Interp`. "Life of the isolate" is reworded throughout to **"life of the
> dedicated isolate (or the top-level program)"** — never the pooled, multi-tenant one.

Why the dedicated-isolate boundary is *real* and not API theater:

- **Memory isolation.** The worker has its own heap, its own `Interp`/`Vm`, its own GC. The plugin
  cannot reach the host's objects by reference — only `Send` bytes cross the structured-clone airlock
  (Workers Spec A §5; `src/worker/isolate.rs:35` "no `Value`, no `Interp` crosses"). An in-process
  API gate can be bypassed by reflection/pointer tricks; a separate heap cannot.
- **The `CapSet` crosses trivially.** It is plain `Send` flags + an `Option` scope — exactly the kind
  of payload the spawn path already carries `Send` (`src/worker/isolate.rs:35`). The dedicated-spawn
  path installs it into the fresh `Interp` *before* running any plugin code. No `!Send` runtime type
  crosses; the keystone needs zero new airlock machinery.
- **Monotone with the in-isolate drop.** Inside the *dedicated* sandboxed worker the plugin can
  `caps.drop` further but never re-grant — and because the isolate is single-tenant, the drop is
  terminal. Denial is one-way at every layer where dropping is permitted.

This is what elevates "capabilities" from a convenience to a security primitive: **the worker
subsystem AScript already shipped is the sandbox; capabilities are the policy that rides it.** FFI —
the most dangerous capability — is precisely the one you most often want to deny to a plugin, so the
two subsystems close the loop.

`run_in_worker(fn, input, opts)` is the explicit, capability-aware dispatch entry. **It does not exist
yet** — it is a *new* deliverable of this spec, a thin caller-side helper that (a) when `opts.caps` is
present, routes to a **dedicated** `spawn_isolate`-backed isolate carrying the reduced `CapSet`; (b)
when absent, falls through to the existing pooled `worker fn` dispatch (`src/worker/pool.rs`). A plain
`worker fn` call (no `run_in_worker`) inherits the **caller isolate's** `CapSet` as the read-only
floor described above.

## 5. Surface syntax & semantics (full API)

### 5.1 `std/ffi`

```
import { ffi } from "std/ffi"

// open + bind a symbol
let [libm, e1] = ffi.open("libm.so.6")          // [ForeignLib, err]
let pow = libm.symbol("pow", [ffi.f64, ffi.f64], ffi.f64)
print(pow.call([2.0, 10.0]))                     // 1024.0

// sized ints marshal over `int`; narrowing is checked
let [libc, e2] = ffi.open("libc.so.6")
let abs = libc.symbol("abs", [ffi.i32], ffi.i32)
print(abs.call([-5]))                            // 5      (int in, int out)

// strings: cstr -> NUL-terminated Bytes, passed as ptr
let strlen = libc.symbol("strlen", [ffi.ptr], ffi.size)
print(strlen.call([ffi.cstr("hello")]))          // 5

// opaque pointers + read-back
let malloc = libc.symbol("malloc", [ffi.size], ffi.ptr)
let p = malloc.call([16])                         // ForeignPtr
// ... use p ...
libc.symbol("free", [ffi.ptr], ffi.void).call([p])

// structs via Bytes + layout
let Point = ffi.struct([["x", ffi.i32], ["y", ffi.i32]])
let buf = Point.alloc()                            // zeroed Bytes, C-aligned
Point.set(buf, "x", 3); Point.set(buf, "y", 4)
someFn.call([buf])                                 // pass buf as ffi.ptr (out-param)
print(Point.get(buf, "x"))
```

Errors: `ffi.open`/`lib.symbol` → **Tier-1 `[value, err]`**; signature/marshalling misuse →
**Tier-2 panic**; a denied `ffi` capability → **recoverable Tier-2 panic** `capability 'ffi' denied`.

### 5.2 `std/caps`

```
import { caps } from "std/caps"

caps.has("ffi")        // -> bool          (query; never grants)
caps.list()            // -> array<string> (currently-granted capabilities)
caps.drop("process")   // irreversible subtraction (top-level / dedicated isolate; refused in a pooled worker fn — §4.5a)
caps.dropAll()         // drop every dangerous capability (same scope rule)
```

No `grant`. `caps.drop`/`dropAll` are the only mutators and are **one-way**. They are **durable only on
the top-level program isolate and on a dedicated `run_in_worker({caps})` isolate**; inside a **pooled**
`worker fn` (which reuses one `Interp` across requests) a drop is **refused** to avoid a cross-request
leak / re-grant (§4.5a). `run_in_worker(fn, input, { caps: { deny: [...], net: {...}, fs: {...} } })` is
the spawn-time restriction that runs the job in a fresh **dedicated** (single-tenant) isolate (§4.5/§4.5a).
All `std/caps` fns register in `std_arity.rs` for `call-arity` checking.

## 6. Sendability & the airlock

A `ForeignPtr`/`ForeignLib`/`ForeignSymbol` is a **`Native` handle**, and `Native` is already on the
**non-sendable** list of the structured-clone serializer (Workers Spec A §5: `Native` resource handles
→ `DataCloneError`-analog Tier-2 panic with the field path). So attempting to send a foreign pointer
to a worker is the existing, correct error: `value of kind Native cannot be sent to a worker at
<path>`. This is the right semantics — a pointer is only valid in the address space that produced it.
The §3.5 guidance ("offload blocking FFI to a worker") therefore means *do the whole native call
inside the worker and return plain data* (`Bytes`/`int`/`Object`), never *pass a live pointer across*.
No new airlock work is required; the existing sendability gate covers all three FFI handles for free.

The `CapSet` carried at spawn (§4.5) is **not** a `Value` — it is a side-channel field on the
`Send` `WorkerRequest` (pooled path: the caller-floor install) / the `Send` dedicated-spawn options
(`run_in_worker({caps})` path), so it does not interact with the structured-clone value serializer at
all. `CapSet` is `Send + Clone` (a small bitset + `Option<FsScope>`/`Option<NetScope>`), satisfying the
existing all-`Send` `WorkerRequest` invariant (`src/worker/isolate.rs:35`).

## 7. Determinism (SP9 interaction — a new effect seam)

FFI is a **new, irreducible nondeterminism/effect seam**, the first the language has that the runtime
cannot model. SP9's determinism context (`src/det.rs`: Record/Replay over the clock/RNG and the
worker/actor boundary, INERT by default) records effects whose *result* it can serialize. An arbitrary
C call's effect is **opaque** — it may mutate process global state, touch hardware, or return a live
pointer — so it **cannot be faithfully recorded/replayed** in general.

Resolution (no silent gaps — `goal.md` gate 6):

- **Outside a determinism context (the default):** FFI runs normally; SP9 is inert; nothing changes.
- **Inside a Record/Replay or `std/workflow` context:** an FFI **call** is a **non-deterministic
  boundary**. The principled choice, matching how SP9 already treats opaque async boundaries
  (`det.rs` `BoundaryOutcome`): on **Record**, the call executes once and its *marshalled return
  value* is recorded as bytes (works for value-returning calls — `int`/`float`/`Bytes`); on
  **Replay**, the recorded bytes are returned **without re-executing the C side**, with a
  signature-match assertion.

  **[SECURITY] Out-param buffers must be recorded too, or replay is refused.** The §3.3 struct/out-param
  pattern makes a **`Bytes` passed as `ffi.ptr`** first-class: a C call commonly *writes through the
  pointer* (filling the buffer) and returns only a **status `int`**. Recording **only the return value**
  would replay that call with the buffer **never written** — the script reads STALE pre-call bytes and
  proceeds as if the call succeeded: a **silent wrong replay** (exactly the gate-6 "no silent gaps"
  violation). Two acceptable resolutions; this spec adopts **(A)** and falls back to **(B)** when a
  buffer is non-recordable:
  - **(A) Record post-call contents of every mutable buffer arg.** For each argument bound to `ffi.ptr`
    whose value is a `Bytes`, the recorder snapshots the **post-call** buffer contents (length + bytes)
    alongside the marshalled return value. On Replay, after returning the recorded status, the recorder
    **writes the recorded post-call bytes back into the live `Bytes`** before control returns to the
    script — so out-params are faithful without re-executing C. (A `Bytes` is sendable/recordable data;
    a `ForeignPtr` is not — see below.)
  - **(B) Refuse replay (loud) when a buffer can't be recorded.** If **any** `ffi.ptr`-typed argument is
    a **`ForeignPtr`** (an opaque foreign address the recorder cannot snapshot or rebind across runs),
    the call is **not replayable**: inside a determinism context it is a **Tier-2 panic**
    `ffi: call with a foreign-pointer out-param is not replayable in a workflow` — a documented, loud
    refusal, never a silent wrong replay. This mirrors the pointer-**return** refusal below.

  A call whose **return type** is **`ffi.ptr` → `ForeignPtr`** is likewise **not replayable** (a pointer
  is meaningless across runs): inside a determinism context such a call is a **Tier-2 panic**
  `ffi: pointer-returning call is not replayable in a workflow`. A new **`ffi-nondeterminism`** lint
  (default Warning, alongside SP9's `workflow-determinism`) flags FFI calls inside a `workflow`/`activity`
  body to steer pointer-returning / opaque-buffer work into an `activity` (which is recorded at its
  boundary, not by the inner call).
- **Differential oracle:** because the whole feature lives at the shared `Value`/`Interp`/stdlib
  layer, `tree-walker == specialized-VM == generic-VM` holds by construction. FFI tests assert
  byte-identical output across all four modes against **deterministic libc/libm calls**
  (`cos(1.0)`, `abs(-5)`, `strlen("hi")`) — pure functions of their inputs, so the differential is
  meaningful (§8). The capability denials are pure control flow and are trivially byte-identical.

## 8. Testing (hermetic)

- **FFI happy path (hermetic — no test fixtures to build):** `dlopen` the platform's **libc/libm**
  (`libm.so.6`/`libSystem`/`msvcrt`) and call **pure** functions whose result is fixed: `cos`,
  `pow`, `sqrt` (`ffi.f64`), `abs` (`ffi.i32`), `strlen` (`ffi.ptr`+`ffi.size`). A small
  platform-resolution helper picks the right library name per OS so the suite is hermetic everywhere.
- **Marshalling:** int narrowing range-check panics (`300` → `ffi.u8`), `u64`-bit-pattern round-trip,
  `f32` precision, `ffi.cstr` NUL-termination, `ffi.struct` offset/alignment get/set, `read_cstr`.
- **Error tiers:** `ffi.open("nope.so")` → Tier-1 `[nil, err]`; bad signature → Tier-2; bad arity at
  `call` → Tier-2.
- **Capability denials:** with `--deny ffi` → `ffi.open` raises `capability 'ffi' denied` (recoverable
  via `recover`); per-scope tests for the CLI flags, the `[capabilities]` manifest table, and
  `caps.drop`; **monotonicity** (after `caps.drop("net")`, `caps.has("net")` is false and stays
  false; there is no API to re-grant). Granular fs-path / net-host allow-within-deny tests.
- **[Gate 10 — DNS egress IS gated (security regression test).]** `net.lookup` / `net.lookupOne` MUST be
  blocked by `caps.drop("net")` and by `--deny net` / `--sandbox`: a program that drops `net` then calls
  `net.lookup("example.com")` raises `capability 'net' denied`, NOT a resolved address list. Asserts the
  dispatch-site gate covers `"net"` (DNS) and there is no per-connect bypass. Companion tests:
  `os.networkInterfaces()` / `os.localIp()` / `os.hostname()` are blocked by `--deny net`;
  `io.readAll()` is blocked by `--deny fs`; ambient `os.platform()` / `os.cpuCount()` are **not** blocked
  (still succeed under `--sandbox`).
- **[Gate 10 — pooled-isolate cap-leak test (the §4.5a soundness regression).]** Dispatch two pooled
  `worker fn` requests to the **same** isolate: request A drops `net` (and asserts the drop is
  **refused**/no-op there, per §4.5a), then dispatch request B to that same isolate and assert it
  **still has `net`** — proving no cap state leaks across pooled requests on the reused `Interp`. Plus:
  a `run_in_worker(plugin, …, {caps:{deny:["net"]}})` on a **dedicated** isolate where the plugin's
  in-isolate `caps.drop("ffi")` is durable for that job and the host isolate is unaffected.
- **The sandboxed-worker test (the keystone):** spawn `run_in_worker(plugin, input, { caps: { deny:
  ["ffi","process"] } })` (a **dedicated** isolate, §4.5a) where `plugin` attempts `ffi.open` → the
  worker observes the denial; assert the host isolate is **unaffected** (still has `ffi`) and that the
  denial crossed the boundary as intended. Plus the existing sendability test extended: returning a
  `ForeignPtr` from a worker → the structured-clone `Native`-not-sendable path error.
- **[SP9 replay — out-param buffer fidelity (security regression).]** Inside a Record/Replay context,
  record a C call that writes a `ffi.ptr` `Bytes` out-param and returns a status `int`; on Replay assert
  the buffer contents are the **recorded post-call bytes**, not stale pre-call bytes (§7A). And: a call
  with a `ForeignPtr` out-param (or a `ForeignPtr` return) inside a determinism context is a **loud
  Tier-2 refusal**, never a silent replay (§7B).
- **GC safety:** a unit test asserting `Value::Native(ForeignPtr)` contributes **zero** traced edges
  (`Value::trace` no-ops for it), guarding gate 4.
- **Four-mode byte-identity:** every FFI example (§ below) runs identically on tree-walker,
  specialized VM, generic VM, and `.aso`-compiled, in both feature configs (`vm_differential.rs`).
- **`--no-default-features`:** with `ffi` off, `import "std/ffi"` is an `unresolved-import`-class
  error (the module is in `STD_MODULES` for the checker but `std_module_exports` returns `None` under
  the cfg) — same discipline as every other feature-gated module. `std/caps` is **core** (not
  feature-gated): capabilities exist even in a bare build (you can still deny `fs`/`net`/`process`).

## 9. Implementation surface & cross-cutting checklist

Per `CLAUDE.md` "Touching syntax" (no *grammar* change here — FFI/caps are pure stdlib + an `Interp`
field + CLI/manifest — so the two-parsers/tree-sitter/formatter rows are **N/A**; everything else
applies). **Each item is a required deliverable.**

**New stdlib modules:**
- `src/stdlib/ffi.rs` — `exports()` (the `ffi.*` type descriptors as values + `open`) and the
  call routing for `open`/symbol/struct/cstr/read_cstr; the libffi CIF build + invoke; the
  marshalling in/out. Feature `ffi`.
- `src/stdlib/caps.rs` — `exports()` (`caps` object) + `call` for `has`/`list`/`drop`/`dropAll`.
  **Core** (no feature gate).
- **Register BOTH in `src/stdlib/mod.rs` both arms** (`std_module_exports` ~line 109 and the `call`
  match ~line 364) **and** add their slugs to `STD_MODULES` (~line 211; `std/ffi` gated-aware,
  `std/caps` unconditional) so the checker resolves the imports in every build.

**`Value`/handles (`src/value.rs`):** add `NativeKind::ForeignLib`, `ForeignSymbol`, `ForeignPtr`
(~line 366) + their `type_name` arms (~line 482). **No new `Value` variant** (sized ints stay `int`).

**Resources (`src/interp.rs`):** add `ResourceState::ForeignLib(libloading::Library)` /
`ForeignSymbol(...)` (the CIF + bound signature + lib ref) / `ForeignPtr(usize)`; they reclaim on
`Drop` (lib `dlclose`); register via the existing `register_resource` (~line 1789). **GC:** confirm
`Value::trace` (`src/gc.rs:176`) leaves `Native` untraced (it already does via `_ => {}`); add the
no-traced-edges test.

**`CapSet` on `Interp` (`src/interp.rs`):** add `caps: RefCell<CapSet>` to the struct (~line 437);
default all-granted in `Interp::new` (~line 788); a `require_cap(Cap, module, func, args, Span) ->
Result<(), Control>` helper returning the recoverable denial panic; **never hold the `caps` borrow
across `.await`** (read a `Copy` snapshot). **Wire the gate at the SINGLE `&self` dispatch site**
`Interp::call_stdlib` (`src/stdlib/mod.rs:276`), immediately before the terminal `match module { … }`
(`src/stdlib/mod.rs:364`), via `required_cap(module, func) -> Option<Cap>` (§4.1/§4.3 — the complete enumeration:
`fs`/`io`/`os`-file→`Fs`, `net*`/`http_server`/`os`-topology→`Net`, `process`→`Process`, `ffi`→`Ffi`,
`env`→`Env`). **Do NOT put the check "at the top of `fs::call`/`env::call`"** — those are **free
functions with no `&self`** (`src/stdlib/fs.rs:70`, `src/stdlib/env.rs:28`) and cannot read
`Interp.caps`. The granular `fs`/`net` second-stage check (§4.4) lives at the net connect/bind entries
(`net_tcp::tcp_connect`/`tcp_listen`, etc.) and the fs path-resolving entries, **short-circuiting to the
bitset when no carve-out is configured** (Gate 12 — no canonicalization on the hot path). A drift-guard
test asserts every resource-acquiring module string has a `required_cap` mapping (Gate 10).

**Worker spawn carries the `CapSet` (`src/worker/`) — two distinct paths (§4.5a):**
- **Dedicated (capability-restricted / untrusted):** `run_in_worker(fn, input, opts)` — a **new**
  caller-side helper — when `opts.caps` is present, spawns a **fresh single-tenant isolate** via
  `spawn_isolate` (`src/worker/isolate.rs:140`), installs `opts.caps` into that isolate's brand-new
  `Interp` **before** running the entry, runs the one job, tears it down. The `Interp` is **never
  reused**, so an in-plugin `caps.drop` is irreversible and cannot leak.
- **Pooled (plain `worker fn`, no `caps`):** add a `caps: CapSet` field to `WorkerRequest` (it is
  `Send`, fits the all-`Send` invariant, `src/worker/isolate.rs:35`) carrying the **caller's floor**.
  `isolate_loop` (`src/worker/isolate.rs:240`) — which REUSES one `Interp` across requests — **installs
  the request's `CapSet` FRESH at the top of each request** (so request B is unaffected by request A;
  no forward leak). `caps.drop` inside a pooled `worker fn` is **refused** (no-op-with-warning /
  recoverable panic), because a durable drop on a shared `Interp` would either leak forward or require a
  re-grant — both unsound (§4.5a). Durable, irreversible `caps.drop` is available only on the top-level
  program isolate and on a dedicated `run_in_worker({caps})` isolate.

**CLI (`src/main.rs`):** add `--deny <list>`, `--deny-net=<scope>`, `--deny-fs=<scope>`, and
`--sandbox` to the `Run` subcommand (~line 17); parse into a `CapSet`; pass to the `Interp` builder.
(REPL may add a `--sandbox` too — optional.)

**Manifest (`src/pkg/manifest.rs`):** parse a `[capabilities]` table (a third owned table beside
`[package]`/`[dependencies]`, ~line 21) into the same `CapSet`; union with CLI denials (monotone).
Hermetic manifest-parse tests (`tests/pkg.rs` style).

**Cargo (`Cargo.toml`):** add `libloading` + `libffi` deps (optional); a `ffi` feature
(`ffi = ["dep:libloading", "dep:libffi"]`) added to the `default` set (~line 107). `std/caps` needs
no dep. Clippy clean in BOTH feature configs.

**Checker/types:** `std_arity.rs` entries (keyed on `(module, name)`, `src/check/std_arity.rs:31`
`std_fn_arity` / `:40` `required_args`) for all `caps.*` and the script-exposed `ffi.*` fns
(`open`, `symbol`, `call`, `struct`, `cstr`, `read_cstr`); the `ffi-nondeterminism` lint (§7) in
`src/check/rules/` (default Warning) added to `rules::ALL`.

- **[Gate 5 — descriptors stay opaque.] Confirmed: the `ffi.*` type descriptors and the three handles
  synth as `Unknown`/opaque** in the inference pass (`src/check/infer/pass.rs`) — they are tagged
  Objects / `Value::Native`, which the lattice already treats as `Unknown` (the gradual escape; only a
  provable `No` ever emits, never `Unknown`). **Invariant:** `examples/**` emits zero `type-*`
  diagnostics in both configs (no new true-positive). A descriptor like `ffi.i32` carries no static
  numeric type into the checker — it is an opaque value arg, exactly like a `std/schema` schema.
- **[Gate 5 — handle-method arity is REACHABLE by the checker.] Confirmed: `symbol.call` arity is
  checkable** because `std_arity` keys on `(module, name)` and the `call-arity` rule resolves
  Native-receiver methods (the `.call`/`.symbol` handle methods) through the same `(module, name)`
  lookup the curated `std_arity` table uses (`src/check/std_arity.rs:40`). The drift-guard test
  (`std_arity.rs:82`) asserts every keyed `(module, name)` is a real export, so adding `("ffi","call")`
  / `("ffi","symbol")` / `("ffi","open")` etc. keeps the rule honest and the methods within reach of
  `call-arity` (too-few-args flagged; `max=None` where the native ignores surplus).

**LSP:** `std/caps` + `std/ffi` symbols flow through the existing `std/*` completion/hover; the new
lint flows through `check::analyze` → diagnostics. No new LSP machinery.

**Determinism (`src/det.rs`):** the FFI-call boundary recording/refusal (§7) — a `DetEvent` reuse for
the marshalled-return-bytes case **plus the post-call snapshot of every `ffi.ptr`-typed `Bytes`
out-param** (recorded contents written back into the live `Bytes` on Replay, §7A), the **`ForeignPtr`
out-param / pointer-return refusal panics** (§7B), and the `ffi-nondeterminism` lint interaction with
`std/workflow`.

**`.aso`:** **no change.** FFI/caps add no opcode and no serialized layout — `ASO_FORMAT_VERSION` is
**not** bumped. (`ffi.*` descriptors and handles are runtime values, never constants.)

**Docs + examples:**
- `docs/content/stdlib/ffi.md` (the marshalling table, handles, the threading caveat, the Tier-1/Tier-2
  error split) and `docs/content/stdlib/caps.md` (opt-out model, three scopes, the sandbox-via-worker
  keystone). **Add both slugs to the `NAV` array in `docs/assets/app.js`** (sidebar + cmd-K derive
  from `NAV` — no entry ⇒ unreachable; per the docs-nav gotcha).
- A capability section in `docs/content/language/` (or the workers page) cross-linking the keystone.
- `examples/ffi_libm.as` (call `cos`/`pow` from libm — hermetic), `examples/advanced/ffi_struct.as`
  (a struct out-param round-trip), `examples/caps_sandbox.as` (`run_in_worker` with a denied `ffi`
  plugin — the keystone demo, fully error-handled). All verified with `target/release/ascript run` and
  exercised by the conformance/differential suites.
- `README.md` stdlib table + `CLAUDE.md` ("Native resource handles" gains the three Foreign kinds; a
  capabilities note); `roadmap.md`; the campaign `goal.md` status tick.

**Tests:** `cli.rs` (the CLI deny flags), `pkg.rs` (`[capabilities]` parse), `vm_differential.rs`
(four-mode FFI examples), `check.rs` (`ffi-nondeterminism` lint), unit tests in `ffi.rs`/`caps.rs`
(§8). Green in both feature configs.

**Unchanged:** the grammar, both parsers, tree-sitter, the formatter, the GC algorithm, the
`Interp` async model, the worker pool/scheduler mechanics, all non-FFI stdlib.

## 10. Scope & rejected alternatives

**In scope:** `std/ffi` (open/symbol/call over libloading+libffi; the `ffi.*` C-type vocabulary
marshalled over `int`/`float`/`Bytes`; cstr/struct/ptr handling; the three `ForeignLib/Symbol/Ptr`
handles); `std/caps` (the five capabilities; opt-out at CLI/manifest/in-code; `require_cap` at the
**single central `call_stdlib` dispatch gate** keyed by module string — covering **every**
resource-acquiring stdlib entry incl. DNS/`io`/`os`-topology, §4.1/§4.3; per-isolate `CapSet`; the
sandbox-via-**dedicated**-worker keystone + the new `run_in_worker` caps option, §4.5a);
the SP9 effect-seam handling; default-on `ffi` feature; docs/examples/tests.

**Out of scope (deferred/reserved):**
- **Callbacks C→AScript** (passing an AScript fn as a C function pointer). Real (libffi closures
  support it), but it crosses back into `!Send` runtime state from a foreign thread — a sharp edge
  deserving its own spec. Reserved.
- **Auto-generated bindings from C headers** (a `bindgen`-style tool). A tooling/DX item, not core
  FFI; possible future `ascript ffi-bind`.
- **Finer-grained capabilities** beyond the five + the fs-path/net-host carve-outs (e.g. per-syscall,
  time/memory quotas). Kept deliberately small for comprehensibility; quotas are a separate concern.
- **A `grant` API / capability re-granting.** Intentionally absent — one-way subtraction is the
  property that makes self-sandboxing trustworthy.

**Rejected:**
- **Opt-IN (Deno-style) permissions.** Rejected on two grounds: (1) it violates `goal.md` pillar 2
  ("batteries included, opt-out not opt-in") — forcing `--allow-read` to read a file is friction for
  the overwhelmingly-trusted common case; (2) opt-in is partly **security theater** in a single-process
  in-memory gate — without the isolate boundary, a determined script bypasses an API check, so the
  flags give false assurance. AScript's answer is *opt-out policy + a real memory boundary* (the
  worker isolate), which is both more ergonomic for the trusted case and genuinely enforceable for
  the untrusted one.
- **FFI default-off (compile-out for safety).** Rejected (§3.6): conflates availability with authority;
  breaks batteries-included; strictly less granular than the per-isolate capability gate.
- **Sized ints (`i32`/`u8`/`u64`) as runtime `Value` kinds.** Rejected per NUM §10 — they are a C-ABI
  marshalling concern expressed *over* `int` at the boundary only; new Value variants would fragment
  the numeric tower, bloat `Value`, and complicate every exhaustive match for zero user benefit.
- **Embedding the foreign pointer directly in `Value`.** Rejected — follows the established
  native-resource pattern (handles in `Interp.resources`, a cheap `Value::Native` reference) so the GC
  stays leaf-opaque and reclamation is deterministic.

## 11. Grounding (verified sources)

- **C FFI for a dynamic language:** LuaJIT `ffi` library (declare C types, `ffi.cdef`/`ffi.new`/
  `ffi.string`); Python `ctypes` (`CDLL`, `c_int`/`c_char_p`, `Structure`) and `cffi` (the
  ABI-vs-API split). AScript adopts the ABI/runtime-loader style (libffi trampoline), with the type
  vocabulary expressed as AScript values rather than a C-syntax DSL.
- **libffi:** the canonical portable C-ABI trampoline (`ffi_prep_cif` + `ffi_call`); the `libffi`
  Rust crate; `libloading` for cross-platform `dlopen`/`LoadLibrary`.
- **Sized-int-over-int marshalling:** NUM §10 (this campaign) — sized ints are an FFI boundary
  concern; the C-int ↔ language-int narrowing/widening contract matches Go cgo and Python ctypes.
- **Capabilities, inverted from Deno:** Deno's `--allow-*` permission flags (the opt-in model AScript
  deliberately inverts to opt-out); OpenBSD `pledge`/`unveil` and Linux `seccomp` for the **one-way
  narrowing** property that makes `caps.drop` trustworthy.
- **The sandbox-via-isolation precedent:** Workers Spec A (this repo) + the shared-nothing isolate
  model (CPython PEP 684, Web Workers) — memory isolation is what turns an API gate into a real
  security boundary; capabilities are the policy layered on the boundary AScript already shipped.
