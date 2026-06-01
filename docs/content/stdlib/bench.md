:::eyebrow Standard library

# std/bench — micro-benchmarking

`std/bench` provides simple micro-benchmarking utilities: measure how long a
function takes per call and compare multiple implementations side by side.

```ascript
import * as bench from "std/bench"
```

Both functions are `async` (they drive async fns to completion per iteration)
and must be `await`ed.

---

## `bench.measure(fn, iterations?) -> stats`

Run `fn` `iterations` times (default **100**) and return a stats object.

| Field | Type | Meaning |
|---|---|---|
| `iterations` | `number` | How many times `fn` ran |
| `totalMs` | `number` | Wall time for all iterations (ms) |
| `avgMs` | `number` | Average time per iteration (ms) |
| `opsPerSec` | `number` | Throughput (1000 / avgMs) |

Timing uses a monotonic `Instant` that wraps the entire loop. If `fn` returns
a `future<T>` (i.e. it is an `async fn`), that future is driven to completion
before the next iteration begins.

```ascript
import * as bench from "std/bench"

let stats = await bench.measure(() => {
  let sum = 0
  let i = 0
  while (i < 1000) { sum = sum + i; i = i + 1 }
  return sum
}, 200)

print(`avg ${stats.avgMs} ms  /  ${stats.opsPerSec} ops/s`)
```

### Async functions

```ascript
import * as bench from "std/bench"
import { sleep } from "std/time"

// Measures wall time including the async sleep — each iteration awaits the body.
let stats = await bench.measure(async () => {
  await sleep(1)
}, 10)

print(stats.iterations)   // 10
print(stats.opsPerSec)    // ≈ 1 000
```

---

## `bench.compare(fns, iterations?) -> array`

Run `bench.measure` on each named function and return results **sorted by
`avgMs` ascending** (fastest first).

```ascript
let results = await bench.compare({
  naive: () => {
    let s = ""
    let i = 0
    while (i < 100) { s = s + "x"; i = i + 1 }
    return s
  },
  join: () => {
    let parts = []
    let i = 0
    while (i < 100) { parts[i] = "x"; i = i + 1 }
    return string.join(parts, "")
  },
}, 50)

for (let r of results) {
  print(`${r.name}: ${r.avgMs} ms  (${r.opsPerSec} ops/s)`)
}
```

Each element in the returned array is:

| Field | Type | Meaning |
|---|---|---|
| `name` | `string` | Key from the input object |
| `avgMs` | `number` | Average time per iteration (ms) |
| `opsPerSec` | `number` | Throughput (1000 / avgMs) |

---

## Notes

- `bench.measure` is designed for **in-process micro-benchmarking**. It does
  not do warmup, statistical analysis, or noise isolation. For rigorous
  benchmarks use a dedicated external harness.
- `iterations` must be a **positive integer**; passing `0` or a fractional
  number raises a Tier-2 panic.
- `opsPerSec` is capped at `1e15` when `avgMs` is exactly 0 (sub-microsecond
  work resolving to zero in f64).
- `std/bench` has no feature gate — it is always available, including under
  `--no-default-features`.
