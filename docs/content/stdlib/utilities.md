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

## std/semver

`std/semver` is a hand-rolled SemVer 2.0.0 parser/comparator plus a
**node-semver-SUBSET** range engine. No dependency.

```ascript
import * as semver from "std/semver"

let [v, err] = semver.parse("1.2.3-rc.1+build.7")
// v == { major: 1, minor: 2, patch: 3, prerelease: ["rc", 1], build: ["build", "7"] }

semver.compare("1.0.0", "2.0.0")            // -1
let [ok, _] = semver.satisfies("1.5.0", "^1.2.3")   // [true, nil]
let [best, _] = semver.maxSatisfying(["1.2.0", "1.3.0", "2.0.0"], "^1.2.0") // ["1.3.0", nil]
```

### `semver.parse(v)`

Parse a strict SemVer 2.0.0 version → `[ {major, minor, patch, prerelease, build}, err ]`.
A leading `v` (`v1.2.3`) is **rejected** (strict SemVer has no `v` prefix), as are
leading zeros in any core/numeric-prerelease field. Build metadata after `+` is kept
but ignored in precedence.

### `semver.valid(v)`

Return whether `v` is a valid strict SemVer 2.0.0 version (`bool`).

### `semver.compare(a, b)`

Compare two versions by SemVer §11 precedence → `-1 | 0 | 1`. Build metadata is
**ignored** (`1.0.0+x` == `1.0.0+y`). A prerelease has lower precedence than the
same release (`1.0.0-rc.1 < 1.0.0`). A **malformed version is a Tier-2 panic** — this
fn assumes already-validated data.

### `semver.sort(versions)`

Return `versions` sorted ascending by precedence. Malformed elements are Tier-2.

### `semver.satisfies(version, range)`

Return `[bool, err]` for whether `version` satisfies the node-semver-subset `range`.
A **malformed range is a Tier-1 `[nil, err]`** (ranges are frequently external data).

**Supported range forms** (documented precisely):

- exact / comparators: `=1.2.3`, `>1.2.3`, `>=1.2.3`, `<1.2.3`, `<=1.2.3`, bare `1.2.3`.
- caret `^`: `^1.2.3` → `>=1.2.3 <2.0.0`; `^0.2.3` → `>=0.2.3 <0.3.0`;
  `^0.0.3` → `>=0.0.3 <0.0.4`; `^1.x`/`^1` → `>=1.0.0 <2.0.0`.
- tilde `~`: `~1.2.3` → `>=1.2.3 <1.3.0`; `~1.2` → `>=1.2.0 <1.3.0`;
  `~1` → `>=1.0.0 <2.0.0`.
- x-ranges / partials: `*`/`x`/empty → any; `1.x`/`1` → `>=1.0.0 <2.0.0`;
  `1.2.x`/`1.2` → `>=1.2.0 <1.3.0`.
- hyphen ranges: `1.2.3 - 2.3.4` → `>=1.2.3 <=2.3.4`; a partial high end becomes
  exclusive (`1.2.3 - 2` → `<3.0.0`).
- space = AND within a comparator set; `||` = OR of sets.

**Prerelease participation rule** (node default, `includePrerelease:false`): a
prerelease version (`1.2.3-alpha`) satisfies a comparator only if that comparator's
tuple has the SAME `[major,minor,patch]` AND itself carries a prerelease. So
`1.2.3-alpha` satisfies `>=1.2.3-0` but NOT `>=1.2.0`.

**Not supported** (documented deferrals): `workspace:`/`npm:` protocols, loose mode,
and `includePrerelease:true` (a recorded future).

### `semver.maxSatisfying(versions, range)`

Return the highest version in `versions` that satisfies `range` → `[string|nil, err]`.
A malformed range is Tier-1; a malformed candidate version is Tier-2.

### `semver.minSatisfying(versions, range)`

Return the lowest version in `versions` that satisfies `range` → `[string|nil, err]`.

## std/diff

`std/diff` is a hand-rolled **Myers O(ND)** line/char diff plus a unified-diff
renderer whose output byte-matches GNU `diff -u`. No dependency.

```ascript
import * as diff from "std/diff"

let patch = diff.unified("a\nb\nc\n", "a\nB\nc\n", { fromFile: "old", toFile: "new" })
// --- old
// +++ new
// @@ -1,3 +1,3 @@
//  a
// -b
// +B
//  c

let hunks = diff.lines("x\ny\n", "x\nNEW\ny\n")
// [ {tag:"equal", ...}, {tag:"insert", lines:["NEW"], ...}, {tag:"equal", ...} ]
```

**Line splitting.** Lines split on `\n` only; a `\r\n` ending is NOT special-cased
(the `\r` stays as the line's last char), so a CRLF file differs from an otherwise
identical LF file (matching GNU `diff` without `--strip-trailing-cr`). An empty string
is **zero** lines; a single `"\n"` is **one** blank line — the two are distinct. A
missing trailing newline is rendered as the `\ No newline at end of file` marker.

**Budget.** Inputs above an internal size budget return a Tier-1 `[nil, err]`
("inputs too large") rather than hang or OOM. Wrong-type arguments (a non-string
input, a non-object `opts`) are Tier-2 misuse.

### `diff.lines(a, b)`

Myers line diff of `a` → `b` as an `array` of hunk objects
`{tag: "equal"|"delete"|"insert", aStart, aEnd, bStart, bEnd, lines}`. `aStart`/`aEnd`
and `bStart`/`bEnd` are 0-based half-open ranges into the respective input's lines.

### `diff.unified(a, b, opts?)`

Render a unified diff (`diff -u` format) of `a` → `b` → `string`. `opts` is an optional
object `{ context? = 3, fromFile? = "a", toFile? = "b" }`: `context` is the number of
equal context lines kept around each change run (change runs whose context windows
overlap are merged into one hunk); `fromFile`/`toFile` label the `---`/`+++` headers.
Identical inputs render the empty string.

### `diff.chars(a, b)`

Myers char-level diff of `a` → `b` as an `array` of hunks (the same shape as
`diff.lines`, with each `lines` entry a single character) — intended for small,
intra-line comparisons.
