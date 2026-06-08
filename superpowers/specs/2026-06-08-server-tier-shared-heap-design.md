# AScript Multi-isolate Server Tier & Shared Read-only Heap — Design (SRV)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** SRV (Serious Language campaign — see `goal.md`)
- **Depends on:** the shipped worker subsystem (`src/worker/` — Workers Spec A
  `2026-06-07-workers-foundation-stateless-design.md` + Spec B
  `2026-06-07-workers-stateful-actors-streaming-design.md`): isolates, the
  structured-clone airlock, the demand-grown pool, per-isolate `!Send` bootstrap.
- **Rebase dependency — NUM then ADT (merge order, cross-cutting #5).** SRV's `SharedNode`
  is written against today's single `Value::Number(f64)` (`src/value.rs:626`) and
  `Value::EnumVariant(Rc<EnumVariant>)` (`src/value.rs:660`). **NUM merges first**
  (`Value::Number → Float`, plus a new `Value::Int(i64)`), so `SharedNode` MUST carry BOTH
  `Int(i64)` and `Float(f64)` (§3.3) — not one `Number(f64)`. **ADT merges next** (it gives
  `EnumVariant` a payload), so `SharedNode::EnumVariant` carries the frozen payload `value`
  (§3.3). SRV's freeze/read/serialize arms therefore land **after** NUM and ADT and rebase
  onto their `Value` shape; `.aso` version is not bumped by SRV (§3.7), so SRV inherits
  whatever `ASO_FORMAT_VERSION` NUM/ADT left and never hardcodes a number. The non-numeric,
  non-enum design (Send-safety, the `Arc`-not-`Cc` acyclic GC story, REUSEPORT, the
  call-position-only `call_shared` hook, no grammar change) is independent of NUM/ADT and
  reviewed sound.
- **Depended on by:** nothing in the campaign requires SRV; BIN (native binary) is
  orthogonal. SRV is a capability + performance leaf.
- **Engines:** both (tree-walker oracle == VM, byte-identical for the order-deterministic
  handler/`shared.freeze` logic; the *parallelism* and the accept loop are I/O, framed in §6).
- **Breaking:** no. `server.serve(opts)` gains a `workers` key (absent ⇒ today's
  single-isolate path, unchanged); `std/shared` and `Value::Shared` are purely additive.

---

## 1. Summary & motivation

AScript can already use all cores for **CPU-bound fan-out** (the shipped `worker fn` pool,
`src/worker/`). Two gaps remain before the language has a *serious server tier*:

### 1.1 The single-core-server gap

The HTTP server is **single-core**. The accept loop (`http_server.rs:900`) and every
spawned per-connection handler task run on **one `LocalSet` on one thread**: the loop
`accept()`s, acquires a `maxConcurrent` semaphore permit (`http_server.rs:913`), and
`tokio::task::spawn_local`s the handler (`http_server.rs:925`) — all `spawn_local`, all the
same thread. This is a deliberate consequence of the `!Send` interpreter (the module doc at
`http_server.rs:5-13` spells it out: hyper's `Service` wants an owned `Send` future that
cannot borrow `&Interp`, so the loop hand-rolls HTTP/1 to keep `&self` live across
accept→dispatch→respond). The `maxConcurrent` semaphore (`http_server.rs:108-112`,
default 256) gives **I/O concurrency** — a slow handler does not head-of-line-block others —
but **not parallelism**: two CPU-bound requests still time-slice one core. A 4-core box runs
a CPU-bound AScript service at ≈25% of the hardware. Every other modern server runtime
(nginx, Envoy, Node `cluster`, Go's `net/http`) spreads accepts across worker
processes/threads; AScript must too.

### 1.2 The deep-clone gap

Workers communicate **only through copied bytes**. The airlock (`src/worker/serialize.rs`)
deep-clones *everything* crossing an isolate boundary — a full structured-clone walk
(`encode`/`decode`, `serialize.rs:360`/`517`), measured at **~1.3 ms per 10k floats**. That
is correct and cheap for per-request *arguments*. It is **ruinous for per-request shared
read-only state**: a 5 MB routing table, a compiled template set, a feature-flag snapshot, a
geo-IP database — anything a request handler must *read* but never mutate — would be
deep-copied into every isolate on every dispatch (or, if shipped as a `const` in the code
slice, deep-copied once per isolate at warmup, ballooning each isolate's resident set by the
table size × `num_cpus`). There is today **no value that crosses the boundary by reference**:
the entire `Value` union is `Rc`/`Cc` and therefore **`!Send`** (`src/value.rs:623`), so by
construction nothing can be *shared* across threads — only copied.

A serious server tier needs an **immutable, shareable snapshot**: build a big read-only graph
once, hand every isolate a pointer to the *same* bytes, pay zero per-request copy. This is the
**shared read-only heap**, and it requires introducing AScript's **first `Send` value**.

### 1.3 What this spec delivers

- **Part A — multi-isolate HTTP serving.** `server.serve({ port, workers: N })` runs the
  accept loop on **N isolates** that each bind the same port via **`SO_REUSEPORT`**, so the
  kernel load-balances accepts across cores. Handlers become `worker fn`s; the
  request/response data crosses the airlock, while per-isolate resources (DB pools) stay local.
- **Part B — `std/shared`.** `shared.freeze(value)` deep-converts a value into an immutable,
  `Arc`-backed graph and returns a **`Value::Shared(Arc<SharedNode>)`** — the first `Send`
  value. It crosses the worker boundary as an `Arc` pointer bump (a fast-path tag in
  `serialize.rs`), not a deep copy; reads are zero-copy on any isolate; mutation is a Tier-2
  panic (it is frozen).

The two parts are complementary: Part A spreads request handling across cores, and Part B is
what makes that *practical* (a per-core isolate must read shared config without paying to copy
it). They share one branch and one review gate.

## 2. The multi-isolate serve model (Part A)

### 2.1 Why SO_REUSEPORT (not single-acceptor-dispatch)

There are two ways to get N isolates serving one port:

1. **Single-acceptor-dispatch.** One thread owns the listening socket, `accept()`s every
   connection, and hands each accepted `TcpStream` to a worker isolate. **Rejected as the
   default.** The accepting thread is a serialization point: every connection funnels through
   one `accept()` and one dispatch hop, and — fatally for *this* runtime — a `TcpStream` is an
   OS fd wrapped in an `!Send` `Value::Native` resource (`src/value.rs:656`,
   `NativeKind::TcpStream`), so handing a live connection to *another isolate* would mean
   either (a) sending the raw fd over a `Send` channel and re-registering it in the target
   isolate's resource table (a real, but fiddly, fd-passing design), or (b) serializing the
   request bytes on the acceptor and the response bytes back — which re-serializes *per
   request through one thread*, recreating the single-core bottleneck one layer up. Either way
   the acceptor caps throughput.

2. **`SO_REUSEPORT` (recommended, the default for `workers: N`).** Each of the N isolates
   opens its **own** listening socket, sets `SO_REUSEPORT`, and `bind`s the **same**
   `host:port`. The **kernel** maintains one load-balancing group and distributes incoming
   connections across the N sockets (Linux hashes the 4-tuple to a socket in the group;
   macOS/BSD round-robin-ish). Each isolate then runs the **same accept-loop body**
   (refactored out of `http_server_serve`, `http_server.rs:832` — see the next paragraph) on
   its *own* socket, on its *own* thread, with its *own* `Interp`/`Vm`. There is **no
   cross-isolate hop per connection**: a connection that lands on isolate *k* is accepted,
   dispatched, and answered entirely on isolate *k*'s core. This is exactly the nginx/Envoy
   "`reuseport`" worker model and Node's `cluster` with `SO_REUSEPORT`.

   **The accept loop is NOT reused unmodified — it is a real refactor (genuine work, not a
   no-op).** `http_server_serve` today takes **`&self`** and pulls its listener out of the
   per-isolate resource table via `self.http_server_mut(id)` (`http_server.rs:871`), where
   `id` is the **per-isolate** server-handle id returned by `setup`/`server.create()`. In the
   multi-isolate path the listener does *not* come from `self.resources` (it is built fresh
   from a REUSEPORT `socket2::Socket`, §2.4), and each isolate has its *own* `id`. So §6
   factors today's loop body into a free async helper
   `accept_loop(&self, listener: TcpListener, id: u64, max_body, timeout_ms, max_concurrent,
   budget: Arc<AtomicUsize>, stop, span)`
   that takes the listener **by value** (the `&self` is kept only for `self.rc()` /
   `handle_connection`, which already run per-isolate) and the handler is resolved by the
   isolate-local `id`. The single-isolate `serve` and each multi-isolate isolate both call
   `accept_loop`; the body (semaphore, per-connection `spawn_local`, drain-on-`maxRequests`)
   is shared, but the *plumbing* (listener source + per-isolate `id` + the shared stop signal,
   below) is new.

`SO_REUSEPORT` wins because it pushes load-balancing into the kernel (lock-free, per-core
accept queues — the documented motivation in the original Linux `SO_REUSEPORT` work and the
Cloudflare/LWN write-ups) and because it requires **zero new cross-isolate fd/stream
transport** — each isolate is just "the single-core server we already ship," replicated N
times with a shared port. The airlock is reused **only** for the handler's request/response
data and the shared heap, never for live sockets.

### 2.2 The cross-platform caveat (locked, surfaced, not hidden)

`SO_REUSEPORT` is **not portable**, and per pillar 1 (no silent deferrals) this is a
documented behavior, not a footgun:

- **Linux, macOS, the BSDs:** `SO_REUSEPORT` is available with kernel load-balancing. This is
  the supported multi-core path. Implementation uses `socket2` to set `SO_REUSEPORT` on each
  isolate's socket before `bind` (tokio's `TcpListener::bind` does not expose the option, so
  the socket is built via `socket2::Socket`, `set_reuse_port(true)`, `bind`, `listen`, then
  `TcpListener::from_std`). **`socket2` is only a *transitive* dependency today** (pulled in
  by tokio/hyper/reqwest, visible in `Cargo.lock` but absent from `[dependencies]`), so SRV
  **adds it as a DIRECT `[dependencies]` entry** in `Cargo.toml`, gated to the `net` feature
  (where the HTTP server lives): `socket2 = { version = "0.5", optional = true }` folded into
  `net = [..., "dep:socket2"]`. `set_reuse_port` is the `socket2` API used; the call is itself
  `#[cfg(unix)]`-gated so the non-REUSEPORT (Windows) build never references it (§2.2 Windows).
- **Windows:** there is **no `SO_REUSEPORT`** with the same semantics (`SO_REUSEADDR` allows
  rebinding but does **not** give kernel connection load-balancing — multiple `accept`ors on
  one `SO_REUSEADDR` socket is undefined/last-binder-wins, not balanced). On Windows,
  `workers: N > 1` **falls back to the single-isolate accept loop** (today's behavior) and
  emits a **one-time `warn`-level `std/log` diagnostic**: *"workers: N requested but
  SO_REUSEPORT is unavailable on this platform; serving single-isolate."* This is honest
  degradation (correct, just single-core on Windows), never a silent drop. A future
  Windows-specific single-acceptor-with-fd-handoff path is an explicitly out-of-scope additive
  option (§8).

The platform decision is made once at `serve` time via `cfg!(any(target_os = "linux",
target_os = "macos", target_os = "freebsd", ...))` plus a runtime probe (a failed
`set_reuse_port` also degrades), so a kernel that refuses the option degrades the same way.

### 2.3 What crosses, what stays isolate-local

Converting a single-isolate handler to a multi-isolate handler is a **user migration**, and
the sendability line (Workers Spec A §5) is exactly the contract:

- **Crosses the boundary (airlocked):** the **parsed request** (the `{method, path, query,
  headers, params, body}` object the server already builds at `http_server.rs:1070-1077`) is
  all sendable kinds (strings + objects), and the **response** (a string, or a `{status,
  headers, body}` object, or a `[value, err]` pair — `value_to_response`, `http_server.rs:508`)
  is likewise sendable. So the request/response *data* round-trips cleanly through the
  serializer. In the `SO_REUSEPORT` model these never actually cross threads per request (each
  isolate accepts + answers locally), but they MUST be sendable because the **handler is a
  `worker fn`** and its dependency closure ships to each isolate at startup (Spec A §6
  code-shipping).
- **Stays isolate-local (per-isolate startup state):** **open DB/redis/socket connections,
  prepared statements, file handles, the `events` bus, `std/sync` channels** — every
  `Value::Native` resource (`Interp.resources`, `src/value.rs:656` / `CLAUDE.md` "Native
  resource handles"). These are **established in each isolate's startup**, inside the isolate,
  and never cross the airlock (they are non-sendable by design — `serialize.rs:133-143`
  rejects `Native` with a path error). This is the canonical pattern, not a limitation: each
  of the N isolates opens its **own** connection pool to Postgres at boot, exactly as N nginx
  workers or N Node `cluster` children each hold their own pool.

### 2.4 Per-isolate startup / state

A multi-isolate server needs a **per-isolate setup hook** — the code that opens this isolate's
DB pool, compiles its templates, loads its routes — run **once when each isolate boots**,
before it begins accepting. The surface (locked in §4) is a `setup` function passed to
`serve`, shipped (like any `worker fn` dependency closure) and run in each isolate's `Interp`
at boot. Its return value (the registered server handle + per-isolate state) lives in that
isolate. Shared *read-only* state that should NOT be re-opened per isolate (the 5 MB routing
table) is exactly the Part B use case: `freeze` it once on the main isolate, hand the
`Value::Shared` to every isolate's `setup` as a sendable argument (a pointer bump, §3).

## 3. The shared read-only heap (Part B)

### 3.1 `shared.freeze(value)`

```
let routes = shared.freeze(load_routes())   // a 5MB immutable routing table
```

`shared.freeze(v)` performs a **one-time deep conversion** of `v` into an immutable,
`Arc`-backed graph:

- It walks `v` (the same sendable-kind set the airlock accepts — `serialize.rs:109`), and for
  every node builds an immutable `SharedNode` (see §3.3) holding `Arc`-shared children.
- The result is a single `Value::Shared(Arc<SharedNode>)`.
- **Non-sendable contents are a recoverable Tier-2 panic at freeze time** with the field path
  (reuse `check_sendable`'s walk + `SendError` path machinery, `serialize.rs:103`): a closure,
  a native handle, a future, a generator inside the value cannot be frozen, and the message
  names where (`value of kind function cannot be frozen at routes.handler`).
- **`freeze` of an already-`Shared` value is the identity** (idempotent; returns the same
  `Arc`).
- Freezing is **O(size of value), paid once.** After that, every read and every cross-isolate
  hand-off is O(1)/pointer-cheap.

### 3.2 Why a new `Value::Shared` variant (not a `Native` handle)

Workers Spec B deliberately modeled actor handles and generator drivers as `Value::Native`
resources to **avoid expanding the `Value` union**. The shared heap is the opposite call —
a **new `Value` variant is the right design here**, and the difference is instructive:

- **`Native` is `!Send` and resource-shaped.** `Value::Native(Rc<NativeObject>)`
  (`src/value.rs:656`) is `Rc`-backed, lives in `Interp.resources`, and relies on
  **deterministic `Drop`** to reclaim an fd. A shared value is the inverse: it has **no OS
  resource**, it is **immutable**, and — the whole point — it must be **`Send`** so it can
  cross threads by pointer. Stuffing a `Send` `Arc` inside the `!Send` `Native` machinery
  would be a category error (and would make `NativeObject` `Send`, which it must never be).
- **Reads must be transparent and fast.** A frozen routing table is read with ordinary
  `table["GET /users/:id"]` / `table.handler` / `table.has(k)` syntax. A `Native` handle reads
  go through `call_native_method` async dispatch; a first-class data variant reads through the
  same `index_get`/`read_member` paths as `Object`/`Array`/`Map` (§3.5), which is both faster
  and the correct mental model ("it's frozen data," not "it's a resource handle").
- **Opacity + immutability are easy to enforce on a dedicated variant.** A single `Shared` arm
  in the mutation paths panics; a single arm in the read paths dispatches to the structural
  reader. There is no risk of a `Shared` accidentally being treated as mutable `Object`
  state.

So: `Value::Shared(Arc<SharedNode>)` — immutable, opaque to mutation, `Send`, read like data.
It is the **only** `Send`-carrying `Value` variant; the union as a whole stays `!Send` (a
single `Send` member does not make the enum `Send` — and we do **not** mark `Value: Send`).

### 3.3 `SharedNode` representation

```rust
/// An immutable, Send, Arc-shared node. A frozen DAG of these is built once by
/// `shared.freeze` and read zero-copy by any isolate. No interior mutability,
/// no Rc/Cc, no Native — every field is itself Send.
pub enum SharedNode {
    Nil,
    Bool(bool),
    // Rebased onto NUM (cross-cutting #5): NUM splits today's single `Value::Number(f64)`
    // into `Value::Int(i64)` + `Value::Float(f64)`, so SharedNode MUST carry BOTH — there is
    // no `Number` arm after NUM. (Pre-NUM this would be one `Number(f64)`; SRV lands after
    // NUM and mirrors its split.)
    Int(i64),                    // Copy, Send
    Float(f64),                  // Copy, Send
    Decimal(Decimal),            // Copy, Send
    Str(Arc<str>),               // Send
    Bytes(Arc<[u8]>),            // immutable byte slice (vs the mutable Rc<RefCell<Vec<u8>>>)
    Array(Arc<[SharedValue]>),   // immutable slice
    Object(Arc<SharedMap>),      // ordered immutable key→SharedValue (IndexMap or sorted Vec)
    Map(Arc<SharedMap>),         // MapKey → SharedValue, canonicalized at freeze
    Set(Arc<SharedSet>),
    // Rebased onto ADT (cross-cutting #5): ADT gives `EnumVariant` a payload, so the frozen
    // node carries the frozen payload `value` (a unit variant freezes with `value: Nil`,
    // matching ADT's representation). enum_name + name identify the variant.
    EnumVariant { enum_name: Arc<str>, name: Arc<str>, value: SharedValue },
    Regex { source: Arc<str> },  // recompiled lazily/per-isolate on read if needed (see §3.5)
    // Instances are frozen by class NAME + frozen fields (the airlock's instance
    // story, serialize.rs:478): reads expose fields; cross-isolate method dispatch
    // is out of scope, exactly as for the airlock.
    Instance { class_name: Arc<str>, fields: Arc<SharedMap> },
}
pub type SharedValue = Arc<SharedNode>;   // (or SharedNode where inlining scalars helps)
```

Both new numeric arms are `Copy + Send` (`i64`/`f64`), so the Send-safety argument (§3.4) is
unchanged. The ADT payload `value: SharedValue` is itself an `Arc<SharedNode>`, so it stays
inside the `Send + Sync` graph. (Numeric MapKey canonicalization for `Map` keys follows NUM's
post-split `MapKey` rule — `Int`/`Float` distinct where NUM makes them distinct, NaN unified —
not a single-`Number` key space.)

The graph is an **immutable DAG by construction** — built bottom-up by `freeze`, never mutated
after.

**Two distinct identity states — a diamond is NOT a cycle.** A single `seen` set conflates two
different conditions and must be split into **two tables**, both keyed by the input container's
identity pointer (`gc::cc_addr` for `Cc` containers, `Rc::as_ptr` for `Bytes`):

1. **`in_progress: HashSet<usize>` — cycle detection (on-stack / being-frozen marker).** A
   pointer is inserted **before** `freeze` recurses into a container's children and removed
   **after** the child `Arc` is built. Encountering a pointer that is **currently in
   `in_progress`** means the recursion has re-entered a node still on its own freeze stack —
   that is a genuine **cycle** (`a.push(a)`), and `freeze` **rejects** it with a recoverable
   Tier-2 panic (`shared.freeze does not support cyclic values at <path>`). Because `Arc` has
   **no cycle collector**, a frozen cyclic graph would leak, so a cycle is a hard error.
2. **`completed: HashMap<usize, Arc<SharedNode>>` — diamond sharing (finished-node map →
   reuse the `Arc`).** When a container finishes freezing, its `Arc<SharedNode>` is recorded
   here. Encountering a pointer **already in `completed`** (but NOT in `in_progress`) means the
   same sub-tree is reachable by **two different paths** — a **diamond**, which is perfectly
   legal in a DAG. `freeze` **reuses the existing `Arc`** (a refcount bump, no re-walk), so a
   diamond stays **one** `Arc` rather than being duplicated. This is the structural-sharing
   property that keeps freeze O(distinct nodes), not O(paths).

The ordering matters: check `in_progress` FIRST (reject) then `completed` (reuse). A node that
is finished is in `completed` and absent from `in_progress`; a node mid-freeze is in
`in_progress` only. A single combined `seen` set cannot tell "I am still building this
ancestor" (cycle → reject) apart from "I already finished building this, on another branch"
(diamond → reuse) — hence two tables. (Rationale for rejecting cycles: a routing table /
template set / config snapshot is never legitimately cyclic; supporting frozen cycles would
require an `Arc`-cycle collector, a non-goal.) Frozen graphs are therefore **acyclic `Arc`
DAGs with preserved internal sharing** — which is precisely what makes the GC story trivial
(§3.6).

### 3.4 Send-safety

`Value::Shared(Arc<SharedNode>)` is `Send` **iff `SharedNode: Send + Sync`**, which holds
because every field is `Send + Sync`: `Arc<str>`, `Arc<[u8]>`, `Arc<[T]>`, `Decimal` (Copy),
`f64`, `bool` — and, recursively, `SharedValue = Arc<SharedNode>`. There is **no `Rc`, no
`RefCell`/`Cell`, no `Cc`, no `Native`** anywhere in the graph. This is enforced structurally
(the type simply contains no non-`Send` field) and asserted with a compile-time
`const _: fn() = || { fn is_send_sync<T: Send + Sync>() {} is_send_sync::<SharedNode>(); };`
(a static `assert_send_sync` test) so a future edit that smuggles an `Rc` in fails to compile.

Crucially, **`Value` itself is NOT marked `Send`** and must not be: the other 20-odd variants
hold `Rc`/`Cc`/`Native` (`src/value.rs:626-671` — `Number(f64)` plus `Rc<…>`/`Cc<…>` payloads)
and are thread-confined. Adding `Value::Shared(Arc<SharedNode>)` introduces the *first* `Send`
leaf into the union, so a **negative compile-time assertion guards future edits**:
`static_assertions::assert_not_impl_any!(Value: Send)`. `Value` is auto-`!Send` *today* purely
because every variant holds a `!Send` `Rc`/`Cc`/`Native` — adding one `Send` member doesn't
flip the enum (it is `!Send` as long as *any* member is `!Send`), but the assertion makes that
**explicit and load-bearing**: if a later edit ever removes the last `Rc`/`Cc` member (or
someone adds a stray `unsafe impl Send for Value`), the build fails here rather than silently
making the whole interpreter `Send` and breaking the `LocalSet` invariant. This requires adding
`static_assertions` as a **dev-dependency** (it is compile-time only — `assert_not_impl_any!`
expands to a `const` check; no runtime cost, no production dep). It is the negative counterpart
to the positive `assert_send_sync::<SharedNode>` above.

Only the `Arc<SharedNode>` *inside* a
`Shared` is `Send`, and only that inner `Arc` is what crosses the boundary (§3.7) — extracted
out of the `!Send` `Value` wrapper, sent as a raw `Send` `Arc`, and re-wrapped in a fresh
`Value::Shared` on the far isolate. The `!Send`-runtime invariant
(`#[tokio::main(flavor="current_thread")]` + `LocalSet`, `CLAUDE.md` "The interpreter") is
untouched: we never make the interpreter `Send`; we make **one immutable leaf payload** `Send`
and pass *that* leaf across the existing `Send` byte/handle channels.

### 3.5 Reads — how a `Shared` dispatches

A frozen value is read with the **same surface operations** as the data it mirrors. The read
paths gain a `Value::Shared` arm that walks the `SharedNode` graph and returns either a scalar
`Value` (materialized cheaply — `Nil`/`Bool`/`Number`/`Decimal`/`Str` from an `Arc<str>` clone)
or **another `Value::Shared`** wrapping the child `Arc` (so descending into a frozen object
field stays zero-copy — you get a `Shared` view of the sub-tree, not a deep copy):

- **Index read** `shared[k]` (`index_get`, `src/interp.rs:5265`; VM `index_get` path): a
  `Shared(Array)` indexes by int → child `SharedValue` re-wrapped as `Value::Shared` (or a
  scalar); a `Shared(Object|Map)` indexes by key → child; out-of-range/missing → `nil`
  (matching `Object`/`Array`/`Map`).
- **Member read** `shared.field` (`read_member`, `src/interp.rs:3567`): a `Shared(Object)` /
  `Shared(Instance)` reads the named field → child wrapped as `Shared`; a missing field → `nil`
  (matching `Object`).
- **Method-read for the read-only method surface:** `.has(k)`, `.keys()`, `.values()`,
  `.len()`/`length`, `.contains(x)` (Set), `for ... of` iteration, `.get(k, default)` — the
  **non-mutating** subset of the Array/Object/Map/Set method tables. These dispatch through a
  small `call_shared_method` (mirroring the `std/schema` / `workflow` **call-site hook**
  pattern, `CLAUDE.md`: when the callee is `Member{object, name}` and the receiver is
  `Value::Shared`, route to `call_shared` for the read-only method set; a **mutating** method
  name — `push`, `set`, `insert`, `delete`, `clear`, `sort`, ... — is the Tier-2 panic of
  §3.8). Iteration yields `Value::Shared` children (or scalars), so a `for r of routes` loop
  walks the frozen array zero-copy.
- **Equality / truthiness / `type_name`:** a `Shared` is **truthy** (it is a container);
  `type_name(Shared(Object))` returns the underlying kind name (`"object"`/`"array"`/`"map"`/
  `"set"`/`"bytes"`/`"instance"`/...) so user code and `instanceof` see it as the data it is
  (a frozen routing object is an `"object"`). `==` on two `Shared`s is **`Arc` identity**
  (like the existing container identity-equality, `src/value.rs:708-723`); a `Shared` never
  compares equal to a non-frozen container (they are distinct values), which is consistent and
  cheap. `is_frozen_value(Shared) == true`, `is_frozen_value` already exists
  (`src/value.rs:263`).

This keeps the user model simple: **a frozen value reads exactly like the value it froze**,
only it is immutable and shareable. (`Regex` reads: a `Shared(Regex)` exposes `.source`;
`.test`/`.match` recompile per isolate from `source` on first use into an isolate-local
`Rc<RegexHandle>` cache — the airlock already recompiles regex from source, `serialize.rs:633`.)

### 3.6 GC interaction (`Arc`, not `Cc` — acyclic by construction)

The cycle-collecting GC (`src/gc.rs`, `gcmodule` Bacon–Rajan over `Cc<T>`) governs the `!Send`
`Rc`/`Cc` heap. `Value::Shared` is deliberately **outside** that regime:

- A `SharedNode` graph is an **immutable, acyclic `Arc` DAG** (§3.3 rejects input cycles). Pure
  reference counting (`Arc`) reclaims it correctly and promptly — **no cycle collector needed**
  because there can be no cycle. This mirrors Erlang's `persistent_term` and Clojure's
  persistent immutable structures: immutable + acyclic ⇒ refcounting suffices.
- **`Value::trace` (`src/gc.rs:176`) gets a `Shared` arm that is a NO-OP** — the GC must
  **never trace into a frozen graph** (the same invariant native handles rely on,
  `src/gc.rs:200-204`). Tracing into `Arc` children would be both wrong (they're a different
  ownership domain, not `Cc`) and pointless (no cycle to find). The arm is a one-liner:
  `Value::Shared(_) => {}`. Because the graph holds no `Cc` and no `Value` *cells*, it
  introduces **zero** new GC edges — it cannot participate in a `Cc` cycle even transitively.
- **Mixed ownership is safe:** a `Shared` may be stored inside a `Cc` `Object`/`Array` (e.g.
  `let config = { routes: shared.freeze(...) }`). The `Cc` container is GC-traced as usual; its
  `Shared` field is a leaf the GC skips. Conversely a `SharedNode` never holds a `Cc`, so no
  `Arc→Cc→Arc` cross-domain cycle is possible (freeze copies values *out* of the `Cc` world into
  the `Arc` world; it never retains a `Cc` handle). The two heaps are cleanly separated.

This is the cleanest possible GC outcome: a new, large, long-lived value kind that adds **no
work** to the cycle collector and **no risk** of a cross-domain leak.

### 3.7 Zero-copy crossing — two transports, by call site

A `Value::Shared` reaches another isolate by **one `Arc` clone**, never a deep walk. There are
**two distinct transport paths**, and the cleanest mechanism differs by call site. First, the
shared sendability fact both paths rely on:

- **Sendability:** `unsendable_kind` (`serialize.rs:109`) gets a `Value::Shared(_) => None`
  arm (it is sendable — in fact it is *the* sendable-by-pointer value). `check_inner`
  (`serialize.rs:154`) treats a `Shared` as a sendable **leaf**: it does NOT recurse into the
  frozen graph (the graph is `Send` whole, by `Arc`).

#### (a) The accept-loop isolate boot (Part A `setup`/`args`) — capture in the closure

The multi-isolate server boots each isolate via `spawn_isolate` (`src/worker/isolate.rs:140`),
whose signature is `spawn_isolate<F>(make_loop: F)` where
`F: FnOnce(Rc<Vm>, mpsc::UnboundedReceiver<Vec<u8>>) -> Fut + Send + 'static`. Its **inbound
channel is `Vec<u8>` only** — there is no per-request byte protocol on the accept-loop isolate
(it runs its OWN accept loop, §2.1; the receiver is unused by the server boot). So the frozen
`args` (and the `Arc<SharedNode>`s they contain) are **captured directly in the `Send`
`make_loop` closure**, NOT shipped as bytes:

- The `setup` worker fn's code slice (`Vec<u8>`, `Send`) and its sendable `args` are prepared
  on the main isolate. Any `Value::Shared` among the args is decomposed into its raw
  `Arc<SharedNode>` (extracted out of the `!Send` `Value` wrapper, §3.4) — a `Send` value —
  and **moved into the closure** passed to `spawn_isolate`. The non-`Shared` args (plain
  sendable kinds: strings, numbers, plain objects) are `encode`d to a `Vec<u8>` (also `Send`)
  and likewise captured. The closure is the existing `make_loop` parameter; it is already
  required to be `Send`, and `Arc<SharedNode>` + `Vec<u8>` are both `Send`, so it compiles
  with no new channel.
- Inside the isolate (on its own thread, holding its own `Rc<Vm>`), `make_loop` reconstructs
  the args: the captured `Arc<SharedNode>`s are re-wrapped as fresh `Value::Shared` (an atomic
  bump apiece), the captured bytes are `decode`d, `setup(...args)` runs to build that isolate's
  server handle, and the accept loop (§2.1 `accept_loop`) begins.
- **Why not the side-vector here.** The originally-drafted `WorkerRequest.shared` side-vector
  (below) rides the **pooled** `dispatch_worker` request struct — but the accept-loop isolate
  is spawned with the **raw `spawn_isolate`** substrate and **never constructs a
  `WorkerRequest`** (it owns its own protocol, per `isolate.rs:140`'s doc). Adding a `shared`
  field to `WorkerRequest` would be dead weight on this path. Direct capture in the `Send`
  closure is strictly simpler, needs no new field, and is the mechanism `spawn_isolate` was
  built for ("Returning the receiver lets the actor/stream driver own the protocol",
  `isolate.rs`).

#### (b) The pooled per-request worker dispatch (handlers-as-`worker fn`) — the byte side-vector

When a handler is itself dispatched as a **pooled** `worker fn` (the per-request path, where
args really do cross over the `Send` `WorkerRequest.args` byte channel,
`src/worker/isolate.rs:49`), a raw `Arc<SharedNode>` is `Send` but is **not bytes**, so the
fast path is a **side-vector of `Arc`s alongside the byte buffer**:

- `encode` emits a new wire tag `TAG_SHARED` (the next free tag after today's `TAG_REF` = 13,
  `serialize.rs:94`) carrying a **u32 index** into a `Vec<Arc<SharedNode>>` "shared table"
  collected during encoding (the `Writer` gains `shared: Vec<Arc<SharedNode>>`). Encoding a
  `Shared` is `shared.push(arc.clone())` (an atomic bump) + writing its index — **no deep walk
  of the frozen graph.** (`encode` returns `(Vec<u8>, Vec<Arc<SharedNode>>)`; today it returns
  `Vec<u8>` — callers thread the second member through.)
- `WorkerRequest` (`isolate.rs:37`) gains a `shared: Vec<Arc<SharedNode>>` field (it is `Send`)
  shipped alongside `args: Vec<u8>`; `dispatch_worker` (`mod.rs:104`) threads it; the inline
  same-thread fallback (`run_slice_inline`, `mod.rs:232`) passes the vector straight through
  (no thread crossing at all).
- `decode` on the far isolate, on `TAG_SHARED`, reads the index and reconstructs
  `Value::Shared(shared_table[index].clone())` — another atomic bump, **zero graph copy**.

This side-vector is genuinely used on path (b); it is **not** used on path (a) (the accept-loop
boot), which is exactly why R1 routes (a) through closure capture instead.

- **Result (both paths):** handing a 5 MB frozen table to an isolate costs **one `Arc` clone**
  (a pointer + an atomic increment), independent of table size — versus a 5 MB structured-clone.
  This is the whole point of the spec.

The `.aso` format is **untouched** by the shared heap (a frozen value is a runtime object, not
a bytecode constant; `freeze` is a runtime call). The `TAG_SHARED` wire tag is a **worker-wire
tag only** (the airlock byte format, `serialize.rs`), NOT an `.aso` constant — so **no
`ASO_FORMAT_VERSION` bump** for Part B. (Part A is pure stdlib/runtime and also needs no `.aso`
change.) SRV inherits whatever `ASO_FORMAT_VERSION` NUM/ADT set by merge order (cross-cutting
#5) and adds nothing to it.

### 3.8 Mutation is a Tier-2 panic — reuse the shipped `frozen_kind` path

A frozen value is immutable. Any **write** to a `Value::Shared` — index-assign `shared[k] = v`,
member-assign `shared.f = v`, or a mutating method (`push`/`set`/`insert`/`delete`/`clear`/
`sort`/...) — is a **recoverable Tier-2 panic** (catchable by `recover`).

**Reuse the shipped message, do NOT invent a divergent string.** The existing frozen-container
guard is `check_not_frozen` (`src/interp.rs:4881`), which reads `frozen_kind(v)`
(`src/value.rs:236`) and raises `cannot mutate a frozen {kind}` where `{kind}` is
`"array"|"object"|"map"|"set"|"instance"`. SRV **extends `frozen_kind` with a `Value::Shared`
arm** that returns the underlying container kind (so a frozen-shared object panics with
`cannot mutate a frozen object`, a frozen-shared array with `cannot mutate a frozen array`,
matching the `object.freeze` story exactly). This means:

- The assignment paths (`ExprKind::Index`/`ExprKind::Member` write arms call `check_not_frozen`
  at `src/interp.rs:4849`/`4854`; the VM `SET_INDEX`/`SET_PROPERTY` opcodes call the same guard
  at `src/interp.rs:5319`) need **no new code** — adding the `Shared` arm to `frozen_kind`
  makes them reject a `Shared` write automatically, with the shipped wording.
- `call_shared_method`'s mutating-name rejection (§3.5) raises the **same** `frozen_kind`-based
  message (it calls `check_not_frozen` on the receiver, or formats `cannot mutate a frozen
  {kind}` from the same helper) — no bespoke "(shared) value" string anywhere.

It aligns with the existing frozen-container story by *literally using its code path* — a
`Shared` is frozen *and* `Send`, where `object.freeze` is frozen-but-`!Send`, but the
mutation-rejection wording is identical.

**A frozen-INSTANCE method call needs its OWN diagnostic (not the mutation panic).** A
`SharedNode::Instance` exposes fields (§3.3) but carries **no methods** (cross-isolate method
dispatch is out of scope, mirroring the airlock's instance story, `serialize.rs:478`). So
`frozen_instance.someMethod()` is **not a mutation** — it is a call for which no method exists
on the decoded shape. Routing it through the §3.8 mutation panic would be misleading (the user
didn't try to write anything). Instead, `call_shared` (the call-site hook, §3.5) emits a
**distinct** recoverable Tier-2 panic: `method '<name>' is not available on a frozen instance
(methods are not shared across isolates; freeze exposes fields only)`. The read-only structural
method set (`.has`/`.keys`/`.len`/…) still works on a frozen instance's fields; only
user-class methods are unavailable, and they get this dedicated message rather than the
mutation one.

## 4. Surface syntax & semantics

### 4.1 `server.serve({ workers: N })` + per-isolate setup

```javascript
import server from "std/http/server"
import shared from "std/shared"
import postgres from "std/postgres"

// Build the big read-only state ONCE, on the main isolate.
const routes = shared.freeze(load_route_table())   // 5MB immutable DAG, Send

// The per-isolate setup runs IN each isolate at boot: open this isolate's own
// connection pool, register handlers. `routes` crosses as an Arc pointer bump.
worker fn boot(routes) {
  let app = server.create()
  let db = postgres.connect(env.get("DATABASE_URL"))   // per-isolate, never crosses
  app.get("/users/:id", worker fn (req) {
    let id = req.params.id
    let route = routes["/users/:id"]    // zero-copy read of the shared table
    return db.query("select ...", [id]) // db is THIS isolate's local pool
  })
  return app
}

// Spread the accept loop across N isolates, each binding the same port via SO_REUSEPORT.
await server.serve({ port: 8080, workers: 0, setup: boot, args: [routes] })
//                                       ^ 0 = num_cpus (like ASCRIPT_WORKERS); N>1 needs REUSEPORT
```

Semantics:

- **`workers` absent or `1`** → today's single-isolate accept loop, byte-for-byte unchanged
  (the whole Part A path is gated on `workers > 1`).
- **`workers: N` (N>1, or `0` = `num_cpus`)** → on a REUSEPORT platform, spawn N isolates
  (reusing the `src/worker` dedicated-isolate substrate, `spawn_isolate`, `isolate.rs:140`).
  The `setup` code slice and its sendable `args` (incl. any `Value::Shared`, as raw
  `Arc<SharedNode>`) are **captured directly in the `Send` `make_loop` closure** handed to
  `spawn_isolate` (§3.7(a)) — *not* shipped over the `Vec<u8>` inbound channel, which the
  accept-loop isolate does not use. Inside each isolate, `make_loop` reconstructs the args
  (re-wrapping the `Arc`s as `Value::Shared`), runs `setup(...args)` to build that isolate's
  **own** server handle (returning a **per-isolate handle `id`**), builds that isolate's
  listener with `socket2::set_reuse_port(true)` + bind + `TcpListener::from_std`, and runs the
  refactored `accept_loop(listener, id, …)` (§2.1) on its own socket. `serve` awaits all N. On
  a non-REUSEPORT platform (Windows) → single-isolate fallback + the one-time warn (§2.2).
- **Global `maxRequests` across N isolates needs a shared `Arc<AtomicUsize>` + a coordinated
  stop (acknowledged nondeterminism).** Today `maxRequests` is a per-loop counter
  (`http_server.rs:841`, a local `usize`). Across N isolates the kernel's
  connection-to-isolate distribution is **nondeterministic** (§5): the TOTAL is bounded but the
  per-isolate split is not (isolate 0 might serve 7 of 10, isolate 1 the other 3, or any
  split). So a single `maxRequests` must be a **shared `Arc<AtomicUsize>`** (the remaining
  budget) handed into every isolate's `accept_loop`: each accepted connection does a
  `fetch_sub`-and-check; when the budget hits 0 the isolate stops accepting and a **shared stop
  signal** (a `tokio::sync::Notify` or a `watch` channel, also cloned into every `accept_loop`)
  is fired so the *other* isolates stop too (otherwise they'd block on `accept()` forever).
  `serve` resolves once all N loops have stopped. **The per-isolate split is explicitly NOT
  asserted** (it is OS scheduling) — only that **exactly `maxRequests` total** connections are
  served across the group; tests assert the aggregate, never which isolate served how many
  (§5). This is the single coordination point Part A adds to the otherwise share-nothing model,
  and it is read-only-budget + edge-triggered stop, no shared mutable script state.
- **`setup`** is a `worker fn` (sendability-checked closure); its `args` are sendable
  (typically the frozen shared state). It runs once per isolate; its return value is the
  isolate's server handle (a per-isolate `id`).
- **Handlers are `worker fn`s.** This is the user migration: a handler that closed over a
  mutable outer `let` or an isolate-local `Native` must be restructured so the handler is a
  `worker fn` (capture rules: params + consts + top-level fns + `Shared` only; `worker-capture`
  checker rule, Spec A §4). Per-isolate `Native` resources are opened inside `setup`, in the
  isolate, and referenced by the handler running in that same isolate.

### 4.2 `shared.freeze` + reads

```javascript
import shared from "std/shared"

let cfg = shared.freeze({ region: "us", flags: { beta: true }, limits: [10, 100] })

cfg.region          // "us"            (scalar read, materialized)
cfg.flags.beta      // true            (descend → Shared view → scalar)
cfg.limits[0]       // 10
cfg.limits.len()    // 2               (read-only method)
cfg.has("region")   // true
for (l of cfg.limits) { print(l) }     // iterates the frozen array, zero-copy

cfg.region = "eu"   // Tier-2 panic: cannot mutate a frozen object  (shipped frozen_kind wording, §3.8)
cfg.limits.push(3)  // Tier-2 panic: cannot mutate a frozen array

shared.freeze(cfg) == cfg              // true (idempotent; same Arc)
shared.isShared(cfg)                   // true   (reflection helper)
```

- `shared.freeze(v) -> Shared` — deep-convert + freeze; idempotent; non-sendable/cyclic input
  → recoverable Tier-2 panic with path.
- `shared.isShared(v) -> bool` — reflection (also `is_frozen(v)` is true and
  `type_name(v)` is the underlying kind).
- No new keyword, no new operator — `freeze` is an ordinary `std/shared` function and a
  `Shared` reads through the existing index/member/method machinery.

## 5. Determinism & the four-mode differential

Per Gate 1, every example must produce **identical output** on tree-walker, specialized VM,
generic VM, and `.aso`-compiled, and `tree-walker == specialized == generic` over the corpus.
SRV splits into a data half (fully byte-identical) and an I/O half (framed):

- **`shared.freeze` + reads are pure `Value`-layer logic** shared by both engines (freeze
  builds an `Arc` graph; reads dispatch through the same `index_get`/`read_member` both engines
  call). So the **`shared.freeze`/read/mutation-panic behavior is byte-identical by
  construction** across all four modes — the freeze examples (§7.3) go straight into
  `vm_differential.rs` like any other corpus program. No determinism seam (no clock/RNG); the
  `Arc` graph is deterministic.
- **The multi-isolate accept loop is I/O**, and parallel accept distribution across isolates is
  inherently nondeterministic in *timing* — the **same class** of recorded nondeterminism the
  async model and Spec A already have (Spec A §9: `gather` preserves order, completion order
  does not). Server byte-identity is therefore asserted on **order-deterministic handler
  logic**, exactly as the worker examples are (Spec A §11.3): the server tests drive a fixed
  sequence of requests and assert the **set/sequence of responses**, not which isolate served
  which connection. A handler whose *output* depends only on its request (a pure transform, a
  keyed shared-table lookup) yields identical responses regardless of which core ran it — that
  is the property the differential asserts. The kernel's connection-to-isolate assignment is
  explicitly NOT asserted (it is OS scheduling, like task completion order).
- Each isolate's `Interp` keeps its **own per-isolate SP9 determinism context** (unchanged,
  Spec A §9): a record/replay run inside one isolate is deterministic; cross-isolate timing is
  the documented nondeterminism.

## 6. Implementation surface & cross-cutting checklist

Per the `CLAUDE.md` "Touching syntax" checklist (mostly N/A — no grammar change) plus the
runtime/stdlib surfaces. **Every item is a required deliverable.**

**Values & core (`src/value.rs`):**
- Add `Value::Shared(Arc<SharedNode>)` (the first `Send` member, after the `Rc`/`Cc` variants
  at `src/value.rs:626-671`). Define `SharedNode` (with **`Int(i64)` + `Float(f64)`** per NUM
  and the **payload `EnumVariant`** per ADT, §3.3) + `SharedValue` + `SharedMap`/`SharedSet`
  (immutable `Arc` containers).
- **All exhaustive `Value` matches** get a `Shared` arm (compile-error-enforced):
  `PartialEq` (`Arc`-identity), `Debug`/`Display` (renders like the underlying kind),
  `is_truthy`, `type_name`, and the frozen helpers **`frozen_kind` (`src/value.rs:236`)** /
  `is_frozen_value` (`src/value.rs:263`) / `freeze_value` (`src/value.rs:249`): a `Shared`
  reports its underlying container kind from `frozen_kind` (so the shipped `cannot mutate a
  frozen {kind}` message applies, §3.8), `is_frozen_value(Shared) == true`, and `freeze_value`
  of a `Shared` is a no-op.
- **GC `Trace` (`src/gc.rs:176`):** `Value::Shared(_) => {}` no-op arm (§3.6); add the positive
  `assert_send_sync::<SharedNode>` compile-time check (hand-rolled `const _: fn()`, as today)
  AND the negative `static_assertions::assert_not_impl_any!(Value: Send)` guard (§3.4 — `Value`
  must stay `!Send` even with one `Send` member; this requires `static_assertions` as a
  **dev-dependency**, compile-time only).

**New module (`src/stdlib/shared.rs`):** `exports()` (`freeze`, `isShared`) + `call(...)`
routing for `"shared.freeze"`/`"shared.isShared"`; `freeze` walks a `Value` into a
`SharedNode` DAG, reusing `check_sendable`'s path-building (`serialize.rs:103`) and the **two
freeze-time identity tables** of §3.3 — `in_progress: HashSet<usize>` (on-stack cycle
detection → reject) and `completed: HashMap<usize, Arc<SharedNode>>` (finished-node diamond
sharing → reuse the `Arc`), both keyed by `gc::cc_addr` / `Rc::as_ptr`. Register in **both**
match arms of `src/stdlib/mod.rs`; declare `pub mod shared` (gate on a new `shared` feature
folded into `default`, like `net`/`log`). **Confirm `--no-default-features` builds:** put the
**variant** + `SharedNode` + read-only dispatch in core `value.rs`/`interp.rs`/`gc.rs`
**unconditionally** (so the bare language has the `Shared` type and can *read* a `Shared`),
and put only the **`shared.*` stdlib functions** behind the feature — mirroring how `Value`
kinds are core but their stdlib lives in feature modules. (The read-only method surface
— `index_get`/`read_member`/`call_shared` arms — must compile with no features on; a
`cargo build --no-default-features` is a required check.)
- `std_arity.rs`: register `shared.freeze` (1 arg) / `shared.isShared` (1 arg).

**Read/write dispatch (`src/interp.rs` + VM):**
- `index_get` (`src/interp.rs:5265`) + `read_member` (`src/interp.rs:3567`): `Shared` arms
  returning child-as-`Shared`/scalar (§3.5).
- Index/member **write** arms call `check_not_frozen` (`src/interp.rs:4849`/`4854`) and the VM
  `SET_INDEX`/`SET_PROPERTY` guard at `src/interp.rs:5319`: extending **`frozen_kind`** with a
  `Shared` arm (above) makes all of these reject a `Shared` write **with no new code**, using
  the shipped `cannot mutate a frozen {kind}` message (§3.8) — do NOT add a bespoke string.
- `call_shared` / `call_shared_method`: the call-site hook for the read-only method set
  (mirroring the `std/schema` hook, `CLAUDE.md`), routed from the `Call` evaluator when the
  receiver is `Value::Shared`. A **mutating** method name → the same `frozen_kind` mutation
  panic; a **frozen-instance user-method** call → the **distinct** `method '<name>' is not
  available on a frozen instance …` diagnostic (§3.8), NOT the mutation panic.
- VM `index_get`/member-read fast paths (inline caches): a `Shared` receiver **deopts to the
  generic `Shared` reader** (it has no shape id / no `ObjectCell`), like any non-cacheable
  receiver — specialized and generic stay byte-identical (Gate 1).

**Worker airlock (`src/worker/serialize.rs` + `src/worker/isolate.rs` + `mod.rs`):**
- `unsendable_kind` (`serialize.rs:109`): `Value::Shared(_) => None` (sendable). `check_inner`
  (`serialize.rs:154`): a `Shared` is a sendable **leaf** — no recursion into the frozen graph.
- **Path (b), the pooled per-request dispatch (§3.7b):** `encode`/`decode` gain `TAG_SHARED`
  (the next free tag after `TAG_REF` = 13, `serialize.rs:94`) + a `Writer.shared:
  Vec<Arc<SharedNode>>` table on encode and a matching index-resolve on decode. `WorkerRequest`
  (`isolate.rs:37`) gains a `shared: Vec<Arc<SharedNode>>` side-vector (it is `Send`);
  `dispatch_worker` (`mod.rs:104`) threads it; the inline fallback (`run_slice_inline`,
  `mod.rs:232`) passes it through. This tag is **worker-wire only**, NOT `.aso` (§3.7) — no
  `ASO_FORMAT_VERSION` bump.
- **Path (a), the accept-loop isolate boot (§3.7a):** does **NOT** use the `WorkerRequest.shared`
  side-vector — the accept-loop isolate is spawned via raw `spawn_isolate` (`isolate.rs:140`,
  inbound `Vec<u8>` only) and **never builds a `WorkerRequest`**. The frozen `Arc<SharedNode>`s
  (and the encoded non-`Shared` args) are **captured directly in the `Send` `make_loop`
  closure**; `make_loop` reconstructs them inside the isolate. No `WorkerRequest` field is
  added for path (a).
- Round-trip test: a frozen value crosses by `Arc` identity on path (b) (assert pointer
  equality is preserved on the same-thread inline path; assert structural equality across
  threads); a `setup` arg crossing on path (a) is asserted via the closure-capture boot.

**HTTP server (`src/stdlib/http_server.rs`):**
- **Refactor (genuine work, R2):** `http_server_serve` (`http_server.rs:832`) today is `&self`
  and takes its listener from the per-isolate resource table via `self.http_server_mut(id)`
  (`http_server.rs:871`), `id` being the per-isolate server handle. Factor today's loop body
  (`http_server.rs:900-935`+) into a helper `accept_loop(&self, listener: TcpListener, id: u64,
  max_body, timeout_ms, max_concurrent, budget: Arc<AtomicUsize>, stop, span)` that takes the
  listener **by value** and resolves the handler by the **per-isolate `id`**. The `&self`
  stays only for `self.rc()` / `handle_connection` (already per-isolate). `serve` parses
  `workers`/`setup`/`args`/`port`; single-isolate `serve` builds the listener from `self`'s
  resource as today and calls `accept_loop`.
- **Multi-isolate path:** when `workers > 1` and REUSEPORT is available, spawn N dedicated
  isolates (`spawn_isolate`, `isolate.rs:140`) — capturing the `setup` slice + frozen
  `Arc<SharedNode>` args in the `Send` `make_loop` closure (§3.7a). Inside each isolate: run
  `setup` → per-isolate handle `id`; build the listener via `socket2::Socket` +
  `set_reuse_port(true)` + bind + `listen` + `TcpListener::from_std`; call `accept_loop` with
  that isolate's `id` and the **shared `Arc<AtomicUsize>` `maxRequests` budget + shared stop
  signal** (§4.1). `serve` awaits all N.
- Global `maxRequests` is a shared `Arc<AtomicUsize>` (decrement-and-check per accept) + a
  shared stop `Notify`/`watch` so reaching the global total stops every isolate; the per-isolate
  split is the documented OS-scheduling nondeterminism (§4.1/§5), only the total is asserted.
- Platform gate + one-time `warn` fallback (§2.2). **New DIRECT dep `socket2`** (transitive
  today, in `Cargo.lock` but not `[dependencies]`): add `socket2 = { version = "0.5", optional
  = true }` and fold `"dep:socket2"` into the `net` feature (R3); the `set_reuse_port` call is
  `#[cfg(unix)]`-gated.

**Checker & types (`src/check/`):** no new grammar, so no new rule for Part A beyond the
existing `worker-capture` (handlers-as-`worker fn` already covered by Spec A's rule). Type
inference (SP10): `shared.freeze(x)` synthesizes a `shared`/opaque-frozen type that reads like
its argument's type (or, minimally, `Unknown` so the gradual gate holds — **`examples/**` must
stay at zero `type-*` in both configs**, Gate 5). Reading a `Shared` field synthesizes the
field type where known, else `Unknown`. Conservative: never emit a false `type-*`.

**LSP (`src/lsp/`):** hover on `shared.freeze` shows it returns a frozen shareable value;
completion offers `shared` as an importable module and `freeze`/`isShared` as its functions.
No semantic-token change (no new keyword).

**Formatter / tree-sitter / editors:** **unchanged** — SRV adds no syntax (`worker`/`fn`/calls
already exist; `serve`/`freeze` are ordinary calls). No `grammar.js` change, no `parser.c`
regen, no `sync-grammar.sh`, no editor-pin bump. (Called out explicitly so a reviewer confirms
the "Touching syntax" steps are genuinely N/A here.)

**`.aso` / `verify.rs`:** **unchanged** — no opcode/serialization-layout change (§3.7). No
`ASO_FORMAT_VERSION` bump.

**Determinism (SP9):** unchanged (§5).

**Docs:** extend the existing workers/concurrency docs page (`docs/content/`, the workers page
added by Spec A/B) with a **"Multi-core servers & the shared heap"** section (REUSEPORT model,
per-isolate setup, `shared.freeze`, the read-only/Send semantics, the Windows caveat); add a
`docs/content/stdlib/shared.md` page **and its slug to the `NAV` array in
`docs/assets/app.js`** (sidebar + cmd-K derive from `NAV` — no entry ⇒ unreachable, the
documented orphan gotcha); cross-link from the `http`/`server` stdlib page and
`modules-async.md`. Update `README.md` (concurrency/stdlib table: multi-core HTTP + `std/shared`);
update `CLAUDE.md` (a SRV entry under "Larger subsystems": the first `Send` value, the REUSEPORT
server tier) and `roadmap.md`.

**Unchanged:** the grammar, both parsers, the formatter, `.aso`, the GC collector itself (only a
no-op trace arm), the single-threaded hot path, the `Value` union's `!Send`-ness as a whole
(only one leaf payload is `Send`).

## 7. Testing & benchmark

### 7.1 Unit & integration tests
- **`shared.freeze`:** round-trips every sendable kind into a `SharedNode` DAG (incl. NUM's
  `Int`/`Float` split and ADT's payload `EnumVariant`); idempotence (`freeze(freeze(x))` same
  `Arc`); **diamond sharing** (a node reachable by two paths → the SAME `Arc`, via the
  `completed` table) is preserved AND is **distinct from cycle rejection** (a `completed`-hit
  must NOT be misreported as a cycle); **cyclic input** (`a.push(a)`, an `in_progress`-hit) →
  the cyclic-value panic with path — a dedicated test feeds BOTH a diamond and a cycle and
  asserts the diamond freezes (one `Arc`) while the cycle panics; non-sendable input
  (function/native/future) → recoverable panic with the field path; both the positive
  `assert_send_sync::<SharedNode>` and the negative `assert_not_impl_any!(Value: Send)` compile.
- **Reads:** index/member/method-read over a frozen array/object/map/set/instance return the
  right scalars and `Shared` sub-views; iteration walks frozen containers; `type_name`/
  `is_frozen`/`==` (Arc identity) behave per §3.5.
- **Mutation panic & frozen-instance method:** index-assign, member-assign, and every mutating
  method on a `Shared` → the shipped `cannot mutate a frozen {kind}` panic from `frozen_kind`
  (catchable by `recover`); a **frozen-instance user-method call** → the **distinct** `method
  '<name>' is not available on a frozen instance …` diagnostic (§3.8) — asserted to be a
  DIFFERENT message than the mutation panic.
- **Airlock:** a frozen value crosses `encode`/`decode` via `TAG_SHARED` as an `Arc` bump (no
  deep copy); a frozen value nested inside a normal sendable object crosses correctly; the
  `WorkerRequest.shared` side-vector is wired through path (b) + the inline fallback; the
  **accept-loop boot path (a)** carries a `setup` `Shared` arg via closure capture (no
  `WorkerRequest`) and reads it correctly in-isolate.
- **`--no-default-features` build:** `cargo build --no-default-features` compiles — the
  `Value::Shared` variant + `SharedNode` + the read-only `index_get`/`read_member`/`call_shared`
  arms are core (no feature), only the `shared.*` stdlib functions are gated (§6).
- **Multi-isolate server (integration, `tests/`, spawning the built binary):** bind `workers: N`
  on REUSEPORT platforms, fire M concurrent requests, assert all M get correct responses and
  that **more than one isolate served** (each isolate tags its responses with an isolate id set
  in `setup`, so the test asserts ≥2 distinct ids appeared → real parallelism, not the
  fallback). **Global `maxRequests`:** with `maxRequests: K` across N isolates, assert **exactly
  K** connections are served IN TOTAL (the shared `Arc<AtomicUsize>` + coordinated stop) and
  that all isolates then halt — but **never** assert the per-isolate split (it is OS-scheduling
  nondeterminism, §4.1/§5). On Windows, a **`#[cfg(windows)]` test** asserts the single-isolate
  fallback + the one-time warn (this path CANNOT be covered by a cross-platform `.as` example —
  Gate 9, see §7.3). A `setup` that opens a per-isolate (mock) resource proves the resource
  never crosses.

### 7.2 Four-mode byte-identity (REQUIRED)
Every `shared.*` example runs identically on tree-walker, specialized VM, generic VM, and
`.aso`-compiled (`vm_differential.rs`, both feature configs) — freeze, reads, and the mutation
panic. The multi-isolate server examples assert order-deterministic response sequences across
modes (§5), wired into the differential harness like the Spec A worker examples.

### 7.3 Example corpus (`examples/` — runnable, doubles as docs & all-modes tests)
- `examples/shared_config.as` — `freeze` a config object, read it, show the mutation panic
  caught by `recover`.
- `examples/advanced/shared_routing_table.as` — a big frozen routing/lookup table read by a
  worker fan-out (the zero-copy-share pattern), fully error-handled.
- `examples/advanced/server_multicore.as` — `server.serve({ port, workers: 0, setup })` with a
  per-isolate setup, a frozen shared lookup table, and a per-isolate (mock or real) connection;
  documented as the production-shaped multi-core server.

> **Gate 9 — the Windows fallback is NOT covered by these examples.** An `.as` example runs
> identically on every platform by design, so it can demonstrate the REUSEPORT path but cannot
> exercise the *Windows single-isolate fallback + warn* (on Linux/macOS the fallback branch is
> never taken). That branch is covered ONLY by the `#[cfg(windows)]` Rust test (§7.1), not by a
> corpus example — stated explicitly so no reviewer assumes an example proves the Windows path.

### 7.4 Benchmark (REQUIRED, reported — extends `bench/`)
A harness under `bench/` (reusing `src/stdlib/bench.rs`) writing a markdown report (sibling to
`bench/PROFILING_RESULTS.md`):
- **Requests/sec across worker counts:** a CPU-bound handler served at `workers` = 1, 2, 4,
  8, … (and the single-isolate baseline); report req/s, **speedup**, and **parallel efficiency**
  (speedup ÷ cores). Expectation (documented, not a hard CI gate — CI core counts vary): clear
  super-1× scaling on REUSEPORT platforms (≳3× on 4 cores for a CPU-bound handler).
- **Shared-heap vs deep-clone per-request cost:** the headline Part B number — hand an isolate a
  read-only table of growing size (10k → 1M entries) (a) as a `shared.freeze`d value (`Arc`
  bump) vs (b) deep-cloned per dispatch (today's airlock). Report the per-hand-off cost vs
  table size — the `Arc` path should be **flat (O(1))** while the deep-clone path is **linear**,
  quantifying the ~1.3 ms/10k-floats clone cost the shared heap eliminates.
- **Freeze cost:** one-time `freeze` latency vs value size (the amortized cost paid once).
- **Gate 12 — the NORMAL index/member path is unchanged by the `Shared` arm.** The read paths
  (`index_get`/`read_member` + the VM inline-cache fast paths) gain a `Shared` match arm; a
  benchmark must prove that a non-`Shared` receiver (ordinary `Object`/`Array`/`Map` indexing
  and member reads — the hot path) is **not regressed** by the added arm. Run the existing
  index/member microbench (or an IC-heavy program from the differential's IC set) **before vs
  after** the `Shared` arm in BOTH `--specialize` (default) and `--no-specialize` (generic VM)
  modes; the added arm must be predictably-not-taken for non-`Shared` receivers (a single tag
  check after the existing fast paths), with no measurable steady-state delta. This is the
  Gate-12 spine: a new value kind must not tax the path that never touches it.

## 8. Scope & rejected alternatives

**In scope:** `server.serve({ workers: N, setup, args, port })` via `SO_REUSEPORT` (Linux/
macOS/BSD) with a single-isolate Windows fallback + warn; the `accept_loop(listener, id, …)`
refactor (R2) with a per-isolate handle id; the **direct `socket2` dep** (R3); the
closure-capture transport of `setup`/frozen args into the `spawn_isolate` accept-loop isolate
(R1); a global `maxRequests` via a shared `Arc<AtomicUsize>` + coordinated stop (with the
acknowledged per-isolate-split nondeterminism); `std/shared` (`freeze`/`isShared`);
`Value::Shared(Arc<SharedNode>)` as the first `Send` value (rebased onto NUM's `Int`/`Float`
and ADT's payload `EnumVariant`), its read dispatch, its mutation panic **reusing the shipped
`frozen_kind` message**, the distinct frozen-instance-method diagnostic, its GC no-op trace,
the negative `assert_not_impl_any!(Value: Send)` guard, and its zero-copy airlock fast path
(path-b side-vector + path-a closure capture); the differential/conformance tests (incl. the
`#[cfg(windows)]` fallback test and the Gate-12 normal-path bench); the benchmark; docs.

**Rejected:**
- **Single-acceptor-dispatch as the default (§2.1).** One accepting thread is a serialization
  point and would require live-`TcpStream`/fd handoff into `!Send` isolates (or re-serializing
  every request through one thread). `SO_REUSEPORT` pushes balancing into the kernel with
  per-core accept queues and **zero** new cross-isolate stream transport. (A Windows
  single-acceptor-with-fd-handoff path is a documented future additive option, not in scope.)
- **Making all `Value`s `Send` (`Rc`→`Arc`, atomic GC).** This is the Workers Spec A
  rejected-alternative (§12 there): a measured 5–32% single-threaded tax (Swift BRC PACT'18;
  CPython PEP 703), structurally unavoidable, and it forfeits replayable scheduling. SRV makes
  **exactly one immutable leaf payload** `Send` — the minimum needed for a zero-copy read-only
  heap — and leaves the `Rc`/`Cc` runtime entirely `!Send`. Isolation still provides
  parallelism; sharing is *read-only and immutable only*.
- **Deep-cloning shared config per request (status quo).** The 5 MB-table-per-request cost
  (~1.3 ms/10k floats × table size) is the gap this spec closes; cloning a large read-only table
  into each isolate on every dispatch is precisely what `shared.freeze` exists to avoid.
- **Frozen cyclic graphs / an `Arc` cycle collector.** `freeze` rejects input cycles (§3.3) so
  the shared heap is an acyclic `Arc` DAG that pure refcounting reclaims — no second collector.
  Routing tables / configs / templates are never legitimately cyclic.
- **Mutable shared state (locks/atomics across isolates).** A shared *mutable* heap reintroduces
  data races and locking — the exact thing the share-nothing model rejects. The shared heap is
  **read-only**; mutation is a panic. Cross-isolate mutable coordination is the actor model
  (Spec B), not the shared heap.
- **Modeling `Shared` as a `Native` handle.** `Native` is `!Send`, `Rc`-backed, resource-shaped
  with deterministic-`Drop` fd semantics — the wrong shape for an immutable, `Send`, GC-trivial
  data value read through ordinary indexing (§3.2). A dedicated variant is correct.

## 9. Grounding (sources to verify in the plan)

- **Immutable shared read-only term store:** Erlang/OTP `persistent_term` (a process-wide,
  copy-free, read-optimized immutable term store — the canonical "freeze a big config once,
  read it everywhere with no copy" precedent); Clojure persistent immutable data structures
  (immutable + structural sharing ⇒ safe concurrent reads, refcount/GC-friendly).
- **`SO_REUSEPORT` for multi-core accept distribution:** the original Linux `SO_REUSEPORT`
  kernel work (Tom Herbert / Willy Tarreau, ~3.9) and the LWN/Cloudflare write-ups (per-core
  accept queues, lock-free balancing, the load-distribution caveats); the BSD/macOS
  `SO_REUSEPORT` semantics; the absence of equivalent kernel load-balancing on Windows
  (`SO_REUSEADDR` is not the same).
- **The nginx/Envoy worker model:** nginx `reuseport` directive (one listener per worker
  process via `SO_REUSEPORT`); Envoy's listener `reuse_port` / per-worker connection balancing.
- **Node `cluster`:** the multi-process "share a server port across workers" model (round-robin
  on most platforms, `SO_REUSEPORT`-style on others) — the closest mainstream analog to
  "`!Send` runtime × N, one port."
- **Structured-clone / copy-across-isolate boundary:** WHATWG structured-clone (shared with the
  worker specs) — the baseline this spec's `Arc` fast path *avoids* for read-only data.
