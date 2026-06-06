# Batteries-Completeness Roadmap

- **Date:** 2026-05-31
- **Status:** Approved (phasing); per-phase design specs to follow.
- **Owner:** Mahmoud Kayyali

## Goal

Move AScript from "small language with a good-but-uneven stdlib" to **genuinely
general-purpose and batteries-included**, closing the gaps that make the stdlib feel
unfinished and adding the primitives and standout features that make developers *choose*
the language. This document is the **sequencing plan**: it commits the order of work and
the cohesion boundaries. Each phase below gets its own detailed design spec
(`docs/superpowers/specs/YYYY-MM-DD-<phase>-design.md`) → implementation plan → review →
`--no-ff` merge, in the order listed.

This complements (does not replace) `docs/superpowers/roadmap.md`, which remains the
milestone-by-milestone record (M1…M17). These phases are the post-M17 "batteries" track.

## Design principles for the phasing

1. **Additive before core.** Stdlib-only native functions (no `value.rs`/interpreter
   change) land before anything that touches the value model, grammar, or tree-sitter —
   risk rises in that direction, so the high-blast-radius work lands late against a stable
   base.
2. **Respect dependencies.** Arg parsing needs `args`; rate-limiting is a semaphore + timer;
   the HTTP framework consumes the validation library; streams consume channels.
3. **Front-load daily-value gap-closing.** The APIs every program touches
   (array/string/math/object) come first.
4. **One cohesive spec per phase.** Each phase is independently valuable, shippable, and
   sized for a single design → plan → implement → review → merge cycle. Cohesion is by
   shared architecture + user story, **not** by priority tier.
5. **No silent deferral of anything important.** Only genuinely niche items are deferred
   (see end); each is a clean additive add later, called out explicitly.

## The arc

completeness → CLI → core types → concurrency → networking → validation → web → language → ergonomics

Risk and dependency point the same way: the additive, dependency-free work is also the
lowest-risk; the core-touching work (Phase 3 types, Phase 8 `match`) is pushed late where a
mistake is cheapest to catch. The one place priority fights risk is `Set`/`Decimal`
(fundamental but core-touching) — resolved by isolating them in their own phase rather than
rushing them into Phase 1.

---

## Phase 1 — Stdlib completeness ("the everyday API")

Purely additive native functions across existing modules. Biggest "stops feeling
unfinished" win; touches no interpreter core. **Size: M** (many small functions).

- **`array`**: `find`, `findIndex`, `some`, `every`, `indexOf`, `flat`, `flatMap`,
  `reverse`, `concat`, `first`, `last`, `unique`, `groupBy`, `chunk`, `zip`, `partition`,
  `sum`, `min`, `max`, `take`, `drop`.
- **`string`**: `startsWith`, `endsWith`, `contains`, `replaceAll`, `indexOf`, `chars`,
  `lines`, `reverse`, `count`, `splitN`.
- **`math`**: trig (`sin`/`cos`/`tan`), `log`/`ln`/`exp`, `sign`, `trunc`, `clamp`,
  `hypot`, `gcd`, `lcm`, stats (`mean`/`median`/`stddev`/`sum`), `randomInt`, `shuffle`,
  `choice`.
- **`object`**: `fromEntries`, `pick`, `omit`, `mapValues`, `deepClone`, `deepEqual`,
  `freeze`.
- **Checksums** (non-crypto): `crc32`, `xxhash` — folded into `crypto` (or `encoding`),
  distinct from the existing crypto hashes.

### ⚠️ Breaking change folded into Phase 1: `string.replace` semantics

`string.replace` currently uses Rust `str::replace`, which replaces **every** occurrence —
i.e. it is doing `replaceAll`'s job (`src/stdlib/string.rs:78`; the unit test at
`string.rs:173` codifies `"a.b.c".replace(".","-") == "a-b-c"`).

**Fix:** `replace(s, from, to)` replaces the **first** occurrence only
(`str::replacen(from, to, 1)`); the new `replaceAll` (added above) takes over
all-occurrence replacement (current behavior).

This is a deliberate, documented breaking change (not a silent drop). Blast radius is
small and fully enumerated:

- `src/stdlib/string.rs:78` — implementation (`replace` → `replacen(.., 1)`).
- `src/stdlib/string.rs:173`, `:191` — unit tests updated; add `replaceAll` tests.
- `docs/content/stdlib/collections.md:121` — the one doc example
  (`string.replace("a.b.c", ".", "-")`) updated; document `replaceAll`.
- No `examples/*.as` program relies on the old behavior (only `regex.replace`, a different
  function, appears in examples — unaffected).

Rationale for first-occurrence semantics: matches the common ergonomic split (a plain
`replace` for the targeted single edit, an explicit `replaceAll` when you mean all), and
makes the two functions non-redundant.

---

## Phase 2 — Program & CLI toolkit

Makes "AScript writes production CLIs" a complete story, riding on the just-landed
`std/log` (which already covers stderr). Mostly additive; `exit`/`args` touch
`main.rs`/`lib.rs`/`interp.rs` lightly. **Size: M.**

- **`exit(code)`** — set process exit status. Design decision to settle in the spec: how it
  interacts with the buffered/streamed output model and native-resource cleanup on the way
  out (and structured-concurrency task teardown).
- **`args` / argv** — read the command line; thread trailing args through clap's
  `Run { file }` so `ascript run prog.as a b c` reaches the script.
- **`stdin` / `input()`** — read piped stdin / prompt the user. The remaining process-I/O
  gap now that logging ships stderr.
- **CLI arg parsing** — flags, subcommands, `--help` generation.
- **URL + query-string parsing** — parse/build URLs and `?a=1&b=2` ↔ map.
- **ANSI color / styling** — lightweight terminal color, separate from full `tui`.

Depends on: `args` before the arg parser.

---

## Phase 3 — Core value types: `Set` + `Decimal`

The only phase(s) that touch `value.rs` + interpreter + `fmt` + tree-sitter. Quarantined
into one focused, carefully-reviewed phase. **Size: L** (may split 3a `Set` / 3b
`Decimal`).

- **`Set`** — membership, dedup, union/intersection/difference. The most-felt collection
  gap (today faked with `Map`/`Object`).
- **`Decimal`** — exact numeric for money and integers > 2^53. `f64`-only is a correctness
  gap, not just convenience. (BigInt subsumed by / decided alongside Decimal in the spec.)

Notes: `value.rs` variant additions ripple to the exhaustive matches in `interp.rs`,
`fmt.rs`, `ast.rs`; literal syntax (if any) ripples to lexer/parser/tree-sitter
(regen `parser.c --abi 14`) and the LSP.

---

## Phase 4 — Concurrency & resilience

Builds on the M17 async engine. These primitives are mutually defined (a rate-limiter *is*
a semaphore + timer), so spec'd together to avoid designing the same primitive twice.
**Size: M.**

- **Channels (mpsc)** — task communication, work queues, fan-in/fan-out.
- **Semaphore** — concurrency cap / backpressure (the runtime already uses one internally
  for the HTTP server).
- **Timers** — `interval`, `debounce`, `throttle`.
- **Resilience** — `retry`/backoff, `rateLimit`.

Explicitly **not** included: threading `mutex`/`waitGroup` — obviated by the single-threaded
`!Send` + `RefCell` model (`gather` already covers "wait for N"; synchronous mutation is
already serialized). See the parallelism-vs-concurrency analysis in the brainstorm.

Depends on: semaphore feeds rate-limit. Provides: channels feed Phase 9 streams.

---

## Phase 5 — Networking & host introspection

Rounds out `net` and gives the program a view of its host — powers health endpoints and the
existing `tui_dashboard` example. Live metrics gated behind a `sysinfo` Cargo feature so
`--no-default-features` stays lean. **Size: M.**

- **DNS lookup** — `tokio::net::lookup_host` (free, no new dep).
- **UDP sockets** — fills the obvious `net` hole (TCP-only today).
- **Network interfaces / local IP.**
- **System metrics** — cpu / mem / disk / load average (via `sysinfo`).
- **Process/host facts** — `hostname`, `pid`, `platform`, `arch`.

Excluded from scope: outbound/public IP (that's an HTTP call to an external service, belongs
in user code, not the stdlib).

---

## Phase 6 — Validation & schema (standout #1)

Promote the existing class `.from` / `T?` typed fields / typed-parse machinery into a
first-class schema/validation library. The hard core already exists, so this is mostly
surface area on it — the cheapest standout feature available. **Size: M.**

- Schema construction: object/array/map/union/optional, primitive validators.
- `parse` / `coerce` with structured **error paths** (field-path on failure).
- `refine` / custom predicates.
- Reuses `validate_into` (`interp.rs`) and the Tier-1 `[value, err]` fusion already powering
  `json.parse(Class)` / `resp.json(Class)`.

Provides: typed handlers for Phase 7.

---

## Phase 7 — HTTP framework (standout #2)

Turns raw `serve` into "pleasant to build APIs in." **Size: M.**

- Router with path params.
- Middleware chain.
- Typed request/response handlers using the Phase 6 validation library.

Depends on: Phase 6 validation.

---

## Phase 8 — `match` pattern extensions (language standout)

`match` **already exists** as a value-returning expression
(`docs/content/language/classes-enums.md:161`) but is **value-only**: literals, `|`
alternatives, enum variants, `_`. No binding, no destructuring, no guards, and no way to
match the `[value, err]` idiom. This phase extends the existing `match` rather than adding
it. A core language feature (grammar / interp / `fmt` / tree-sitter / LSP), so it lands
after the stdlib is solid — but **Size: M, not L**, because it leans on pattern grammar the
language already ships (array/object destructuring, ranges) plus an `if`-guard on the arm;
it is mostly wiring existing patterns into match arms with per-arm binding scope.

New capabilities (each reuses existing machinery):

- **Binding patterns** — capture the scrutinee or sub-parts by name (`x if x < 0 => ...`).
- **Range patterns** — `1..=9 => ...` (reuses the `..=` range work).
- **Array patterns** — shape + rest binding (`["move", x, y]`, `["echo", ...rest]`,
  `[first, ..]`), reusing array-destructuring grammar.
- **Object / instance patterns** — match on shape, bind fields
  (`{ method: "GET", path } => ...`), reusing object-destructuring grammar.
- **Guards** — `pattern if cond => ...`.

**Headline: the `[value, err]` Result idiom.** Pattern-matching a Result pair is the single
most idiomatic operation in the language; today it is `if err != nil` boilerplate:

```ascript
const [user, err] = json.parse(body, User)
const msg = match [user, err] {
  [u, nil]  => "welcome ${u.name}",     // success: bind u
  [nil, e]  => "bad request: ${e}",     // failure: bind e
}
```

This is the strongest justification for the phase and revises the earlier "nice more than
needed" read: binding patterns directly remove Result-handling boilerplate.

**Scope limit (not in this phase):** AScript enums are *opaque* — no payloads (the docs say
"model with a class hierarchy"). So Rust-style ADT matching (`Shape.Circle(r) => ...`) is
**out of scope** here; the object/instance pattern above covers the equivalent need.
Payload-carrying enums + their match arms are a natural high-value follow-on once binding
patterns exist.

---

## Phase 9 — Streams & test-runner depth

Ergonomics polish that benefits from everything underneath being in place. **Size: M.**

- **Streams / `pipe`** — composable abstraction unifying generators + Phase-4 channels +
  readers (builds on the existing `stream_pipeline.as` example).
- **Test-runner depth** — assertions, snapshot testing, benchmarks on top of `test()`.

Depends on: Phase 4 channels (for streams).

---

## Dependency graph (non-obvious edges)

- Phase 2: `args` → arg parser.
- Phase 4: semaphore → rate-limit.
- Phase 6 validation → Phase 7 router (typed handlers).
- Phase 4 channels → Phase 9 streams.

Everything else is independent and re-orderable.

## Deferred — genuinely niche only (revisit after Phase 9)

None of these are fundamental to general-purpose use; each is a clean additive add later.
**To be revisited as a batch once all nine phases are complete** — at which point real usage
will show which (if any) have become important enough to promote into their own phase.
Listed explicitly so the set is not lost and any can be pulled up sooner on request:

- Additional DB clients: Postgres / MySQL / Redis (sqlite ships today).
- Serialization breadth: XML / HTML parsing, MessagePack / CBOR.
- Compression breadth: brotli / zstd / tar (gzip / deflate / zip ship today).
- Text **diff**, **LRU cache**, **event emitter / pub-sub**, templating engine.
- i18n depth: pluralization, timezone conversion, message formatting (intl ships
  number/currency/date/case/compare today).

## Status snapshot at time of writing (2026-05-31)

Recently landed (post-M17, on `main`): object/spread/rest destructuring, live `print`
streaming (`OutputSink::Live`), **`std/log`** (leveled structured logging → stderr),
optional `;` separators in class bodies, REPL multi-line continuation. `std/log` closes the
"write to stderr" gap; everything else in this roadmap is open.
