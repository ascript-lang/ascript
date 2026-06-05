:::eyebrow Standard library

# Utilities — LRU, events, templates

Three small, dependency-free in-process utilities. All are **core** modules — they
build and run under `--no-default-features`, like `std/set` and `std/map`.

## std/lru

A bounded least-recently-used cache. `lru.new(capacity)` returns a handle; methods
mutate it in place. Setting beyond `capacity` evicts the least-recently-used entry;
`get` and `set` mark an entry most-recently-used. Keys may be any hashable value.

```ascript
import { new } from "std/lru"

let cache = new(2)
cache.set("a", 1)
cache.set("b", 2)
cache.get("a")        // promotes "a" to most-recently-used
cache.set("c", 3)     // evicts the LRU entry ("b")
cache.has("a")        // true
cache.has("b")        // false
cache.len()           // 2
```

| Method | Returns | Notes |
|---|---|---|
| `new(capacity)` | handle | `capacity` is a number ≥ 1 |
| `get(key)` | value \| nil | marks the entry MRU |
| `set(key, value)` | nil | inserts/updates, marks MRU, evicts the LRU if full |
| `has(key)` | bool | does NOT change recency |
| `delete(key)` | bool | true if it was present |
| `clear()` | nil | drop all entries |
| `len()` | number | current entry count |
| `keys()` | array | keys in LRU→MRU order |

## std/events

An event-emitter / pub-sub. `events.new()` returns an emitter; listeners are called
in registration order.

```ascript
import { new } from "std/events"

let bus = new()
bus.on("greet", (name) => print(`hi ${name}`))
bus.once("boot", () => print("booting"))   // fires exactly once
let fired = await bus.emit("greet", "Ada") // calls listeners; returns the count
```

| Method | Returns | Notes |
|---|---|---|
| `on(event, fn)` | nil | register a listener |
| `once(event, fn)` | nil | one-shot listener (removed after it fires) |
| `off(event, fn?)` | number | remove a listener by identity, or all for `event`; returns the count removed |
| `await emit(event, ...args)` | number | call each listener (awaiting `async fn` listeners) in order; returns the count invoked |
| `listenerCount(event)` | number | listeners registered for `event` |

`emit` awaits each listener in registration order, so errors surface
deterministically; a listener panic propagates as a Tier-2 panic.

## std/template

Minimal `{{name}}` string templating — distinct from AScript's own `${…}` string
interpolation. `template.render(tmpl, data)` substitutes `{{path}}` placeholders
(dotted paths supported) against `data` (an object / instance / map).

```ascript
import * as template from "std/template"

let [text, err] = template.render(
  "Hi {{name}}, your plan is {{account.plan}}",
  { name: "Ada", account: { plan: "pro" } },
)
// text == "Hi Ada, your plan is pro"
```

- **Missing key → Tier-1 error** (strict): `render` returns `[nil, err]` whose
  message names the unresolved path. No silent empty substitution.
- **Raw output** (no HTML escaping — output is not assumed to be HTML).
- Whitespace inside the braces is trimmed (`{{ name }}` == `{{name}}`).
- **No loops or conditionals** — that would be a templating language; out of scope.
  A literal `{{` with no closing `}}` is a Tier-1 error.
