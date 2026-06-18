::: eyebrow Standard library

# Resilience policies

`std/resilience` is AScript's backend-hosting policy kit. Policies are plain tagged objects
(`{ __resil: "..." }`) — the same tagged-object model as `std/schema`. Method-style calls
(`breaker.call(fn)`, `limiter.tryAcquire()`) are **call-position only**: a bare
`breaker.state` member read returns the stored config field, while `breaker.state()` (with
parentheses) invokes the method hook.

All policies are **per-isolate** — under `server.serve({ workers: N })` each isolate has its
own independent policy state, which is the standard per-replica circuit-breaking model. A
policy crosses a worker boundary loudly (the non-sendable `__local` marker), rather than
silently duplicating its counters into a divergent twin.

```ascript
import * as resilience from "std/resilience"

let b = resilience.breaker({ window: 20, minCalls: 10, failureRate: 0.5 })
let [v, err] = b.call(fetchData)     // err.code == "breaker-open" when rejected
print(b.state())                      // "closed" | "open" | "halfOpen"
print(b.stats())                      // { calls, failures, rejected }
b.reset()                             // back to closed, window cleared
```

## Circuit breaker

`resilience.breaker(opts) -> breaker policy`

Protects a dependency by tracking call outcomes in a sliding count window and opening the
circuit when failures exceed the threshold.

| Option | Default | Description |
|---|---|---|
| `name` | `"default"` | Label for metrics and error messages |
| `failureRate` | `0.5` | Open when window failure fraction ≥ this (0, 1] |
| `window` | `20` | Sliding window size — last N calls |
| `minCalls` | `10` | No verdict before this many calls in the window |
| `cooldownMs` | `30000` | Open → halfOpen after this long (ms) |
| `halfOpenMax` | `3` | Max concurrent probes while halfOpen |

### Methods

| Method | Returns | Description |
|---|---|---|
| `b.state()` | `string` | `"closed"` \| `"open"` \| `"halfOpen"` |
| `b.stats()` | `object` | `{ calls, failures, rejected }` counters |
| `b.reset()` | `nil` | Clear state back to closed (testing/ops hook) |
| `b.call(fn)` | `[value, err]` | Run `fn()`, tracking outcome; `err.code == "breaker-open"` when rejected |

### Error codes

| Code | Raised by |
|---|---|
| `"breaker-open"` | `b.call()` while open or half-open budget exhausted |

## Coming soon

Additional policies — `limiter`, `keyedLimiter`, `bulkhead`, `retry`, `memoize`, deadline
propagation, singleflight, metrics handler, and health checks — are being implemented in
subsequent milestones.
