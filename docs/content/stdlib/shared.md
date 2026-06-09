:::eyebrow Standard library

# std/shared — the shared read-only heap

`std/shared` gives AScript a **shared, immutable, zero-copy snapshot** that can be
read by every worker isolate without being copied. `shared.freeze(v)` deep-converts a
value into an immutable, `Arc`-backed graph and returns a `Value::Shared` — the
runtime's **first `Send` value**. A frozen 5 MB routing table crosses to each isolate
as one pointer bump (an atomic increment), not a per-dispatch deep copy.

```ascript
import * as shared from "std/shared"
```

This is the data half of the **server tier** (the other half is multi-core HTTP
serving — see [Workers & parallelism](../language/workers)). Workers normally
communicate only through *copied bytes* (the structured-clone airlock), which is cheap
for per-request arguments but ruinous for large per-request **read-only** state. The
shared heap is the answer: build the read-only graph once, hand every isolate a
pointer to the same bytes, pay zero per-request copy.

---

## `shared.freeze(value) -> Shared`

Deep-convert `value` into an immutable frozen graph and return it as a `Shared`.

```ascript
import * as shared from "std/shared"

let cfg = shared.freeze({ region: "us", flags: { beta: true }, limits: [10, 100] })
```

- **Immutable.** Any write to a frozen value — `cfg.x = 1`, `cfg[k] = v`, or a mutating
  method (`push`/`set`/`insert`/`delete`/`clear`/`sort`/…) — is a recoverable Tier-2
  panic (`cannot mutate a frozen {kind}`), catchable by `recover`.
- **`Send` (shareable).** A `Shared` is the only AScript value that can cross a worker
  isolate boundary **by reference**. Handing it to an isolate costs one `Arc` clone,
  independent of the graph's size.
- **Idempotent.** `freeze` of an already-frozen value returns the **same** value
  (`shared.freeze(cfg) == cfg` is `true`).
- **O(size), paid once.** Freezing walks the value once; afterwards every read and
  every cross-isolate hand-off is O(1).

### What can be frozen

Every **sendable** kind: `nil`, `bool`, `int`, `float`, `decimal`, `string`, `bytes`,
`array`, `object`, `map`, `set`, enum variants (incl. payloads), `regex`, and class
instances (by class name + frozen fields — methods are not shared across isolates).

A value that contains a **non-sendable** part — a function/closure, a native handle
(an open socket, a DB connection), a future, or a generator — **cannot be frozen**.
`freeze` raises a recoverable Tier-2 panic naming the field path:

```ascript
let [_, err] = recover(() => shared.freeze({ handler: () => 1 }))
print(err.message)   // value of kind function cannot be frozen at .handler
```

### Cycles vs diamonds

A frozen graph is an **acyclic `Arc` DAG**. `freeze` distinguishes two cases:

- A **diamond** — the same sub-tree reachable by two paths — is preserved as **one**
  shared `Arc` (structural sharing; freeze stays O(distinct nodes), not O(paths)).
- A **cycle** (`a.push(a)`) is **rejected** with a recoverable Tier-2 panic
  (`shared.freeze does not support cyclic values at <path>`). An `Arc` graph has no
  cycle collector, so a frozen cycle would leak — a routing table / config snapshot /
  template set is never legitimately cyclic, so this is a hard error, not a footgun.

---

## `shared.isShared(value) -> bool`

Reflection: `true` only for a `Shared`. (`type_name(v)` reports the *underlying* kind —
a frozen object is still `"object"` — and the runtime's `is_frozen` helper is also
`true` for a `Shared`.)

```ascript
print(shared.isShared(cfg))   // true
print(shared.isShared({}))    // false
```

---

## Reading a frozen value

**A frozen value reads exactly like the value it froze** — only it is immutable and
shareable. Scalar reads materialize to ordinary values; descending into a frozen
sub-object yields another `Shared` view (so the descent stays zero-copy).

```ascript
import * as shared from "std/shared"

let cfg = shared.freeze({ region: "us", flags: { beta: true }, limits: [10, 100] })

cfg.region          // "us"     (scalar read, materialized)
cfg.flags.beta      // true     (descend → Shared view → scalar)
cfg.limits[0]       // 10       (index a frozen array)
cfg.limits.len()    // 2        (read-only method)
cfg.has("region")   // true     (membership)

for (l of cfg.limits) { print(l) }   // iterates the frozen array, zero-copy
```

The read-only method surface — `.has(k)`, `.keys()`, `.values()`, `.len()`/`length`,
`.contains(x)` (Set), `.get(k, default)`, and `for … of` iteration — works on a frozen
value. The **mutating** methods do not (they raise the frozen panic above).

> A frozen **instance** exposes its fields but **not its methods** (methods are not
> shared across isolates). Calling one raises a distinct recoverable panic:
> `method '<name>' is not available on a frozen instance (methods are not shared
> across isolates; freeze exposes fields only)`.

---

## The cross-isolate pattern

Build the read-only state once on the main isolate, freeze it, and hand the `Shared`
to each worker. The frozen graph crosses as a pointer bump, not a copy — so a large
table is read across all cores without ballooning each isolate's resident set.

```ascript
import * as shared from "std/shared"
import * as task from "std/task"
import * as array from "std/array"

let routes = shared.freeze({
  "GET /users": "listUsers",
  "GET /users/:id": "getUser",
  "POST /users": "createUser",
})

worker fn resolve(table, key) {
  let handler = table[key]
  if (handler == nil) { return `404 ${key}` }
  return `${key} -> ${handler}`
}

async fn main() {
  let keys = ["GET /users", "POST /users", "GET /missing"]
  let futures = array.map(keys, (k) => resolve(routes, k))   // routes crosses by Arc bump
  print(await task.gather(futures))
}
await main()
```

For the multi-core HTTP server that pairs this with `SO_REUSEPORT` accept loops, see
[Workers & parallelism → Multi-core servers & the shared heap](../language/workers).

---

## Notes

- The shared heap is **outside the cycle-collecting GC**: a frozen graph is an
  immutable, acyclic `Arc` DAG, so pure reference counting reclaims it (no cycle
  collector needed, no GC tracing into it). A `Shared` stored inside an ordinary
  (GC'd) object is a leaf the collector skips; the two heaps stay cleanly separated.
- Equality on two `Shared`s is **`Arc` identity** (like other container identity
  equality); a `Shared` never compares equal to a non-frozen container.
- `std/shared` is part of the default feature set; the `Value::Shared` type and its
  read-only dispatch are core (they build under `--no-default-features`), while the
  `shared.*` functions are behind the `shared` feature.
