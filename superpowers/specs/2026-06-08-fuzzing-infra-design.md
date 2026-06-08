# AScript Differential Fuzzing & Property Testing — Design (FUZZ)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** FUZZ (continuous infrastructure of the Serious Language campaign — see `goal.md`)
- **Depends on:** nothing structurally; **stood up alongside NUM** so the numeric tower (its biggest
  early bug surface) is fuzzed from day one. The grammar-aware generator tracks the live grammar, so
  every syntax-touching spec (NUM, ADT, IFACE) extends it in the same PR.
- **Depended on by:** **BIN** (native single-binary distribution) — Gate: BIN must not land until (a)
  the **P0 `.aso` reader-clamp fix is merged** (§2.2; a *live* unbounded-allocation bug today, not a
  hypothetical) and (b) the `.aso` reader + verifier fuzz target has met the **quantified sustained-
  clean bar** (§4.1: ≥ 7 consecutive ≥ 4 h nightly runs crash-free since the last reader/verifier
  change + ≥ 90 % `read_*` coverage), because a shipped native binary parses attacker-influenceable
  `.aso` bytes (`goal.md` BIN: *"Must land after FUZZ hardens the `.aso` reader"*).
- **Engines:** both, plus the generic VM — the whole point is the **three-way differential**
  (`tree-walker == specialized-VM == generic-VM`) turned into a continuous generator-driven oracle.
- **Breaking:** no. This is CI/test infrastructure; it adds no language surface and changes no runtime
  behavior. It is the operational form of the **"No bugs"** pillar (`goal.md` pillar 1).

---

## 1. Summary & motivation

AScript holds a **rare correctness asset**: two fully independent execution engines that are required
to be byte-identical — the tree-walking interpreter (the permanent differential oracle,
`src/lib.rs:180` `run_source_exit` / `:175` `run_source`) and the bytecode VM, which itself runs in two
modes that must also agree (specialized, `src/lib.rs:332` `vm_run_source`; generic / `--no-specialize`,
`src/lib.rs:345` `vm_run_source_generic`). The existing differential harness
(`tests/vm_differential.rs`) already asserts `tree-walker == specialized-VM == generic-VM` over a
**fixed corpus** — the `examples/**` tree plus hand-written cases
(`assert_three_way_matches`, `tests/vm_differential.rs:5284`; the whole-corpus net,
`three_way_whole_corpus_generic_equals_specialized_equals_treewalker`, `:5316`). That corpus is
finite and human-authored: it tests the programs we *thought* to write.

A differential oracle is the strongest possible test fixture — **any** program for which the three
engines disagree is a *guaranteed* bug, with no need to know the right answer in advance — but it is
wasted on a fixed corpus. FUZZ turns the oracle into a **continuous bug-finding machine**: a
grammar-aware generator emits an unbounded stream of random *valid* AScript programs, runs each on all
three engines, and asserts identical output / termination / panic. The generator is biased toward the
edges where engines historically diverge (numeric overflow boundaries from NUM §9, deep nesting near
the recursion guard, closures and capture-by-value, `match` narrowing, generics-in-types parsing).

The second motivation is a **security surface, not just a correctness one**. Two subsystems parse
*untrusted bytes*:

- **`.aso` deserializer + verifier** (`src/vm/aso.rs` `Chunk::from_bytes`, `:453`;
  `src/vm/verify.rs` `Chunk::from_bytes_verified`, `:782`). When **BIN** ships native binaries, the
  embedded `.aso` is an attack surface — a malicious or corrupted `.aso` must produce a clean
  `AsoError`/`VerifyError`, **never** a panic, out-of-bounds read, uncontrolled allocation, or UB.
  **There is a LIVE unbounded-allocation / abort(OOM) bug here TODAY** (cross-cutting review finding
  #1, REVIEW-FINDINGS-2026-06-08): byte *reads* are bounds-checked (`Reader::take`, `aso.rs:354`, with
  `checked_add` + `Truncated`; length reads narrow `u32→usize` via `try_from`, `:392`), **but the
  collection pre-allocations are not.** Every `reserve(n)` / `with_capacity(n)` in the reader
  (`read_chunk` `aso.rs:571`–`610`, then `read_value`/`read_proto`/`read_type`/`read_class_proto`
  `:705`/`:724`/`:769`/`:918`/`:1201`/`:1219`/`:1240`/`:1266`/`:1296`/`:1387`+/`:1487`/`:1585`) passes an
  attacker-controlled `u32` length **with no `.min(r.remaining())` clamp** — and `Reader` has no
  `remaining()` method at all. A crafted 9-byte `.aso` claiming `len = 0xFFFF_FFFF` forces a multi-GB
  allocation and SIGABRTs *before* `verify` ever runs. The worker serializer already shows the exact
  fix (`serialize.rs:564`, `Vec::with_capacity(len.min(r.remaining()))`, with `remaining()` at `:306`).
  So this is **find-and-fix**, not "harden the already-defensive reader": FUZZ owns the reader-clamp
  fix as an explicit, BIN-gating deliverable (§2.2), and then proves it stays fixed against `2^N`
  mutated inputs via coverage-guided byte fuzzing.
- **The structured-clone worker serializer** (`src/worker/serialize.rs` `encode`/`decode`), which the
  workers spec calls *"the airlock that keeps the runtime `!Send`"*. It already has hand-written
  hardening tests (truncation, unknown tag, dangling ref, huge-length-no-allocate;
  `serialize.rs:988`–`1038`) — exactly the shapes a fuzzer generalizes and finds the rest of.

**Honesty up front (§9):** fuzzing finds bugs; it does **not** prove their absence. Every target below
documents what it does and does not cover. No silent caps.

## 2. The targets

Four targets, each with a stated **oracle or invariant** (the property that, if violated, is a bug).

### 2.1 Differential program fuzzer (the headline target)

- **Oracle:** for a generated valid program `P`,
  `run(tree-walker, P) == run(specialized-VM, P) == run(generic-VM, P)`, compared on the
  **deterministic projection** of (captured stdout, exit code, panic message + span) — exactly the
  tuple `assert_three_way_matches` already normalizes (`tests/vm_differential.rs:5289`). A fourth mode,
  **`.aso`-compiled** (`build_file` → `run_aso_file`, `src/lib.rs:356`/`389`), is added for the
  *seed corpus + shrunk reproducers* (it is slower — full compile + serialize + verify + run — so it is
  not run on every generated case, but every saved crash and every example is checked four-mode, the
  `goal.md` Gate-1 standard).
- **What a divergence means:** a guaranteed bug in the compiler, a VM opcode, a specialization guard
  (generic ≠ specialized → a fast-path guard is wrong, per `CLAUDE.md`), `.aso` round-trip, or the
  tree-walker oracle itself. The generator produces a **minimal reproducer** via shrinking (§3.3).
- **Reused entry points (the oracle hooks, already public):** `ascript::run_source_exit`,
  `ascript::vm_run_source`, `ascript::vm_run_source_generic`, `ascript::build_file` +
  `ascript::run_aso_file`. FUZZ adds **no new engine API** — it drives the same seams the corpus
  harness drives.

### 2.2 `.aso` deserializer + verifier fuzzer (SECURITY-CRITICAL — gates BIN; FIXES a live P0 bug first)

> **This target opens with a real bug fix, not a clean-sheet hardening.** As of 2026-06-08 the
> invariant below is **FALSE**: `Chunk::from_bytes` **does** allocate unboundedly on crafted input
> (the unclamped `reserve`/`with_capacity` enumerated in §1 and cross-cutting review finding #1). The
> first work item of this target — and the FUZZ campaign's **first catch** — is to make the invariant
> *true*, then keep it true.

- **The P0 reader-clamp fix (explicit FUZZ deliverable, gates BIN — sequence it as the campaign's
  pre-req).** Clamp every reader pre-allocation against bytes remaining, mirroring the worker
  serializer's already-shipped fix (`serialize.rs:564`/`:306`):
  1. Add a `remaining(&self) -> usize` method to `aso.rs`'s `Reader` (it has none today) returning
     `self.buf.len() - self.pos`.
  2. Replace every `c.<field>.reserve(n)` (`read_chunk`, `aso.rs:571`–`610`) and every
     `Vec::with_capacity(n)` / `IndexMap::with_capacity(n)` in the `read_*` recursion
     (`:705`/`:724`/`:769`/`:918`/`:1201`/`:1219`/`:1240`/`:1266`/`:1296`/`:1387`+/`:1487`/`:1585`) with the
     `.min(r.remaining())`-bounded form. Each element is ≥1 byte, so the clamp can never under-reserve
     a *valid* chunk, and the push loop still errors on the first short read — byte-identical accept
     behavior, no abort. (Per-element minimum size is conservative: a 0-byte element kind does not
     exist in the format; the bound is a *reservation* ceiling, never a length cap.)
  3. This fix lands in the **FUZZ scaffolding PR** (it has no NUM dependency) and **gates BIN** — BIN
     must not land until this is merged *and* the `.aso` target has run its sustained nightly clean
     campaign (§4.1) over the fixed reader. Standalone-pre-req framing: if BIN is scheduled before the
     full FUZZ scaffolding is ready, the clamp fix may ship as its own pre-req bugfix PR carrying *only*
     the reader change + the §7 huge-length-no-allocate self-test; the fuzz target then adopts it as a
     committed regression seed.
- **Invariant (true AFTER the fix above):** `Chunk::from_bytes(arbitrary_bytes)` and
  `Chunk::from_bytes_verified(arbitrary_bytes)` **always** return `Ok(chunk)`, `Err(AsoError)`, or
  `Err(FromBytesVerifiedError)` — **never** panic, never read out of bounds, never allocate
  unboundedly, never loop forever, never exhibit UB. (`from_bytes_verified` lives in `verify.rs:782`
  and returns `Result<Chunk, FromBytesVerifiedError>` — a wrapper enum `{Decode(AsoError),
  Verify(VerifyError)}`, `verify.rs:789`, **not** a bare `AsoError`/`VerifyError`.) A returned `Ok`
  chunk additionally must **pass `verify`** (the `from_bytes_verified` contract) and must be
  **runnable without the VM tripping a `debug_assert`/panic** on it (we run a bounded number of steps
  on accepted chunks — see "runnable-accept" below).
- **Why coverage-guided:** the reader is a recursive descent over a tagged tree (`read_chunk` →
  `read_proto` → `read_value` → `read_expr` → `read_type` …, `aso.rs:565`+). The branch space (every
  tag byte, every length field, every nesting combination) is astronomically larger than the corpus.
  libFuzzer/AFL coverage feedback drives the mutator into the deep `read_*` arms that hand-written
  tests never reach.
- **Two sub-modes:**
  1. *From-scratch bytes* — `arbitrary` / raw libFuzzer bytes straight into `from_bytes`. Mostly
     rejected early (bad magic, version mismatch), but exercises the header + early `Reader` paths.
  2. *Structure-aware mutation* — seed the corpus with **real `.aso` files** built from the example
     corpus (`build_file` over `examples/**`), then let the mutator flip bytes. This reaches deep into
     valid-ish structures (a real proto tree with one corrupted length / tag) where the dangerous bugs
     live. This is the higher-value mode.
- **"Runnable-accept" check:** when `from_bytes_verified` returns `Ok`, run the chunk on the VM under
  a **step/time budget** and assert it terminates with a `RunOutcome`/`Control`, not a host panic. A
  *verified* chunk that crashes the VM is a verifier gap (a missing check in `verify.rs`), which is
  itself a security finding. (Bounded to avoid the halting problem — see §9.)

### 2.3 Structured-clone round-trip fuzzer

- **Invariant (round-trip):** for an arbitrary **sendable** `Value` graph `v` (incl. cycles),
  `decode(encode(v), interp)` is **value-equal** to `v` under AScript's own equality — including Map
  key canonicalization (−0.0→+0.0, NaN unification) and cycle topology (shared sub-objects stay shared,
  not duplicated). This generalizes the workers spec's per-kind round-trip requirement (§11.1). The
  **NUM-dependent sub-cases** — `integral-float→int` key folding (NUM §3.3) and `encode∘decode preserves
  Int` (NUM §9) — reference `Value::Int`, which does not exist until NUM; like the §2.4 numeric
  properties they **land in the NUM PR** (extending the round-trip property the FUZZ scaffolding PR
  ships for the pre-NUM `Value::Number` kinds). The non-numeric round-trip (str/array/object/map/set/
  bytes/cycles) and both invariants below are FUZZ-scaffolding-PR work.
- **Invariant (rejection safety):** for an arbitrary **byte buffer**, `decode(bytes, interp)` returns
  `Ok` or `Err(SendError)` — never panics, never over-allocates. The existing
  `decode_rejects_truncated_buffer` / `decode_huge_length_does_not_allocate`
  (`serialize.rs:988`/`1018`) become the seed; the fuzzer generalizes them.
- **Invariant (sendability honesty):** `encode(v)` succeeds **iff** `check_sendable(v)` returns `Ok`
  (`serialize.rs:103`). A value the checker calls sendable must encode; an un-sendable kind
  (`Function`/`Native`/`Future`/`Generator`) must produce a `SendError` with a field path, never a
  panic and never silent data loss. This couples the static check to the actual serializer.

### 2.4 Property-test suite (in-tree, `proptest`)

Structured properties that are sharper as explicit invariants than as differential programs. Run as
ordinary `cargo test` (deterministic, seeded, fast) — they are part of the normal suite, **not** a
separate fuzz job.

- **NUM numeric tower (the seed from NUM §9) — *lands in the NUM PR, not the FUZZ scaffolding PR.***
  These four properties all reference `Value::Int(i64)` / `Value::Float(f64)`, which **do not exist
  until NUM merges** (today `src/value.rs` has only `Value::Number(f64)`, NUM design `:56`/`:255`).
  Sequencing: **NUM merges first**; these property tests are written and committed *inside the NUM PR*
  (they are part of NUM's acceptance — its biggest early bug surface fuzzed from day one), reusing the
  FUZZ scaffolding (`tests/property.rs`, the shared generator, the edge-biased numeric distribution)
  that the FUZZ scaffolding PR lands ahead of / alongside NUM. The FUZZ scaffolding PR itself ships the
  differential generator + the `.aso`/serialize/parser targets and the **non-numeric** properties (GC,
  formatter idempotence, parser round-trip, front-end agreement); it does NOT reference `Value::Int`.
  - *Exact comparison:* `∀ i: i64, f: f64 : (Int(i) == Float(f))` agrees with exact mathematical
    equality (the 2^53 boundary, NUM §3.3 / §9.1).
  - *Map-key consistency:* `∀ numeric a, b : (a == b) ⟺ (MapKey::from(a) == MapKey::from(b))`
    (integral floats fold to int keys, NUM §3.3).
  - *Overflow edges:* the §3.2 arithmetic table — `+ - * **` and unary `-` and `<<` **trap** at the
    i64 boundary; `+% -% *%` **wrap** and never trap; `/0` and `%0` panic. Property form: random
    operand pairs near `i64::MIN/MAX` produce the documented panic-or-value, identically on all three
    engines (this property is *also* a differential generator bias, §3.2).
  - *Division/remainder identities:* `(a/b)*b + a%b == a` for the int path where it must hold; sign of
    `%` follows the dividend.
- **GC (no leak / no double-free over random cyclic graphs):** generate a random object/array/map
  graph with arbitrary cycles (`Value::trace` mirrors them, `CLAUDE.md` Values §), drop the roots,
  `gc::collect()`, and assert the live count returns to baseline (no leak) and a second `collect()` is
  a no-op (no double-free / no resurrection). This exercises the `gcmodule` Bacon–Rajan collector on
  shapes the corpus never builds. (Interaction note in §9 — proptest's own arena vs the thread-local
  GC heap.)
- **Formatter idempotence:** `fmt(fmt(x)) == fmt(x)` for generated valid source `x` — the formatter
  reaches a fixed point in one pass. Reuses the generator (§3.1) as the source of `x`; a second
  property checks `parse(x)` and `parse(fmt(x))` produce **structurally equal** ASTs (formatting never
  changes meaning).
- **Parser round-trip:** `parse(print(ast))` is structurally equal to `ast` for a generated `ast`
  (the AST-level generator, §3.1) — print an AST to source, re-parse, get the same AST back. This
  pins the `Display`/`fmt`/parser triangle (the same triangle NUM §8 worries about for `5.0`
  surviving a round-trip).
- **Both front-ends agree (parser differential):** `parse_legacy(x)` and `parse_cst(x)` agree on
  accept/reject and on structure for generated `x` — generalizing `tests/frontend_conformance.rs`.
  (Caveat: the documented 1-column caret-span offset between front-ends, `CLAUDE.md` SP1 trade-off, is
  excluded from the comparison projection — compared on structure + message, not caret column.)

## 3. The generators

### 3.1 Grammar-aware AST generator (the core asset)

A **typed AST generator** that emits *well-formed* AScript — not random bytes hoping to parse. It
builds an `ast`-level tree (the legacy `src/ast.rs` types, which both `print`/`Display` and the parser
round-trip against) and renders it to source via the existing `Display`/`fmt` path. Generating valid
programs is what makes the **differential** oracle usable: output-only fuzzing (§10) finds crashes but
cannot find a *correctness divergence*, because a program that panics in all three engines is not a
bug — only a program that **runs differently** is, and most random byte strings don't run at all.

Design:

- **`arbitrary`-driven, recursion-budgeted.** Implement generation against the `arbitrary::Arbitrary`
  unstructured-bytes source so the **same** generator serves both `proptest` (in-tree, shrinking) and
  `cargo-fuzz` (coverage-guided): libFuzzer's mutated bytes become AScript programs, and coverage
  feedback steers the byte stream toward new grammar productions. A depth/size budget (decremented per
  recursion) guarantees termination and keeps programs runnable.
- **ONE generator, dev-only-wired, with NO `fuzz/`→in-tree dependency (must-fix #3).** The risk to
  avoid: making the `ascript` crate depend on the `fuzz/` crate (or pulling `arbitrary` into the
  production graph) just to share the generator — that would taint plain `cargo build` and
  `--no-default-features`. The **concrete mechanism:** the generator source lives at a single canonical
  path, `src/fuzzgen/mod.rs` (+ submodules), wrapped in its own crate-level `#[cfg(any(test,
  fuzzing))]` gate so it compiles into the `ascript` crate **only** for `cargo test` (the `test` cfg)
  and **never** in a normal/production or `--no-default-features` build. Both consumers include the
  *same file*, not a crate dependency:
  - the **in-tree proptest** (`tests/property.rs`) reaches it via the test-cfg-gated module (it is part
    of the `ascript` crate under `cfg(test)`);
  - the **libFuzzer targets** in the separate `fuzz/` workspace member include the identical source via
    `#[path = "../../src/fuzzgen/mod.rs"] mod fuzzgen;` and build with `--cfg fuzzing`
    (cargo-fuzz sets `fuzzing` automatically), so the gate admits it there too.

  Because `fuzz/` `#[path]`-includes the file rather than depending on `ascript`-with-a-fuzz-feature,
  there is exactly **one** generator definition (no drift), and neither `arbitrary` nor the generator
  reaches a non-test build of `ascript`. (`arbitrary` stays a `[dev-dependencies]` entry of `ascript`
  for the proptest side and a normal dependency of the isolated `fuzz/` crate for the libFuzzer side —
  it appears in the production graph of neither.) This mirrors the existing isolated-workspace pattern
  used for `tree-sitter-ascript/` so a plain `cargo build` pulls in neither libFuzzer nor nightly.
- **Scope-correct by construction.** The generator threads a symbol environment so every identifier it
  emits is **in scope** (params, prior `let`/`const`, top-level fns), every `const` is never
  reassigned, every called function exists with a matching arity, and `break`/`continue`/`return`/
  `yield` appear only where legal. This keeps programs *valid* (they compile) instead of mostly-
  rejected — maximizing the fraction that actually reaches the three engines.
- **Deterministic-by-construction (§7).** No clock/RNG/wall-time/iteration-order-dependent constructs
  are emitted (no bare `task.race`, no `Set`/`Map` iteration printed without a sort, no `uuid.v4`/
  `time.now`), OR they are emitted only inside the SP9 determinism context. The generated program's
  observable output must be a pure function of the program text, or the differential is meaningless.
- **Edge-biased distribution (the value of a *custom* generator over blind `arbitrary`):** weighted
  toward known divergence-prone regions —
  - **Numeric edges (NUM priority):** literals at/near `i64::MIN/MAX`, `2^53±1`, `0`, `-0.0`, mixed
    int/float arithmetic, division/remainder by small and zero divisors, wrapping vs checked ops,
    bitwise/shift at amounts {0, 63, 64, -1}. Directly seeds the §2.1 differential from NUM §9.
  - **Deep nesting** toward (but under) the recursion/expr-nesting guards (`MAX_CALL_DEPTH`,
    `EXPR_NEST_LIMIT`, `CLAUDE.md` SP3) — the boundary where the two engines' depth accounting must
    match exactly (`tests/vm_differential.rs:6874`+ already pins the hand-written cases).
  - **Closures & capture-by-value** (the resolver's `captured && mutated` vs `!mutated` split,
    `CLAUDE.md` Values §) — per-iteration loop freshness is a classic divergence farm.
  - **`match`** with ranges/guards/Option-C bind-vs-compare, and **generics in type position**
    (`map<int, array<int>>`, `future<array<int>>` — the NUM §3.4 `>>`-split, which both parsers must
    agree on).
- **Two granularities:** an **expression** generator (feeds `assert_vm_matches_treewalker`-style
  single-expression checks, `tests/vm_differential.rs:24`) and a **program** generator (fns, classes,
  control flow → the full four-mode pipeline).

### 3.2 Byte / structure-aware mutators (for the parsing targets)

- **`.aso` (§2.2):** seed corpus = real `.aso` built from `examples/**`; libFuzzer mutates bytes.
  Coverage feedback over the `read_*` recursion tree is what makes this find the deep bugs.
- **Worker serialize (§2.3):** two generators — (a) an `arbitrary` *sendable `Value`* generator for
  the round-trip property, and (b) raw/mutated bytes into `decode` for the rejection-safety property.
- **Source parser (`src/syntax/parser.rs` `parse`, `:170`; legacy `parser.rs`):** raw bytes →
  `parse` must never panic (it already degrades to error nodes via `p.error` + `p.bump`, `:177`);
  fuzz confirms no input panics either front-end, and the AST-generator output exercises the *valid*
  path.

### 3.3 Shrinking (minimal reproducers)

A divergence on a 400-node random program is nearly useless; a 3-line reproducer is a bug report.

- **proptest** shrinks structurally for free (it owns the `Strategy`), so the in-tree properties and
  the proptest-hosted differential shrink to a minimal AST automatically.
- **cargo-fuzz** ships `cargo fuzz tmin` (libFuzzer input minimization) for the byte targets. For the
  AST-over-`arbitrary` differential target, minimizing the **input bytes** monotonically simplifies
  the generated program (fewer bytes → shallower budget → smaller AST), so `tmin` yields a small
  reproducer; we additionally re-render the shrunk program to canonical source via `fmt` for the
  committed regression test.
- **Lifecycle (§5):** the shrunk reproducer is rendered to a `.as` snippet (or raw bytes for the
  parser targets) and **committed as a permanent regression test** in the normal suite.

## 4. CI & lifecycle

### 4.1 Two cadences

- **Per-PR (fast, time-boxed):** a `fuzz` CI job runs each cargo-fuzz target for a **short fixed
  budget** (e.g. 60–120 s/target) starting from the **committed corpus** (so PRs re-exercise every
  historical crash + accumulated interesting inputs). The proptest properties (§2.4) run inside the
  ordinary `cargo test` step (they are deterministic and already part of the suite) — they are *not*
  the time-boxed job. A PR that introduces a divergence reproducible from the existing corpus fails
  immediately and cheaply.
- **Nightly (deep):** a scheduled workflow runs each target for **minutes–hours**, persists the grown
  corpus + any crash artifacts, and (for `.aso`) constitutes the **sustained clean run that gates
  BIN**. A nightly crash opens/updates a tracking issue and uploads the minimized reproducer as an
  artifact.

#### BIN gate — the concrete "sustained clean" bar (quantified, must-fix #4)

"Sustained clean" is not "the fuzz job is green." BIN may not land until the `.aso` target meets ALL
of:

- **≥ 7 consecutive nightly runs** of **≥ 4 hours each** on the `aso_roundtrip` target with **zero
  crashes** (no panic / OOM / OOB / timeout-as-hang) — and the streak counts only runs **since the most
  recent change to any `read_*` function in `aso.rs` or to `verify.rs`** (a reader/verifier edit resets
  the counter, because it can reintroduce the class of bug this gate exists to catch).
- **A coverage floor over the reader:** the accumulated corpus must reach **≥ 90 % line coverage of the
  `read_*` family in `src/vm/aso.rs`** (`read_chunk`/`read_value`/`read_proto`/`read_type`/
  `read_class_proto`/`read_expr` and their callees), measured by the nightly's coverage pass. Below the
  floor the campaign hasn't actually exercised the deep arms, so "no crash" is uninformative.
- **The P0 reader-clamp fix (§2.2) is merged** and its huge-length-no-allocate self-test (§7) is green
  in the normal suite. (Without the clamp the very first deep-length mutation aborts — the gate cannot
  even start accumulating a streak.)

These three numbers (7 runs / 4 h / 90 % `read_*` coverage) are the literal BIN handshake; BIN's spec
cites this section rather than the loose "green in CI" phrasing (BIN review finding R5).

### 4.2 Corpus & crash artifact store

- The seed/grown corpus lives under `fuzz/corpus/<target>/` (git-tracked seeds; the nightly-grown
  superset cached via `actions/cache` keyed per target, so coverage accumulates across runs without
  bloating the repo).
- **`.aso` seed corpus is REGENERATED on every `ASO_FORMAT_VERSION` bump (must-fix #5).** The `.aso`
  seeds are real files built by `build_file` over `examples/**`, so they carry the *current* format
  version (`ASO_FORMAT_VERSION`, `aso.rs:105`, today `18`). The reader's first act is a version check
  that rejects mismatches early — so a corpus of *stale-version* seeds only ever exercises the
  version-reject path, leaving the deep `read_*` arms (exactly where the clamp bug and its kin live)
  unfuzzed. **Therefore:** any PR that bumps `ASO_FORMAT_VERSION` (which `CLAUDE.md` already mandates
  for any opcode/layout change) MUST regenerate `fuzz/corpus/aso_roundtrip/` from `examples/**` in the
  same PR. This is enforced by a CI check (the cached nightly corpus is *additionally* keyed on
  `ASO_FORMAT_VERSION`, so a bump invalidates the cache and forces a fresh seed build) and called out in
  the `CLAUDE.md` `.aso`-versioning checklist. The coverage floor (§4.1) would otherwise silently
  collapse on the next bump.
- Crash inputs are uploaded as CI artifacts and, once minimized, **moved into the repo** as a
  regression fixture (next section).

### 4.3 The crash → regression-test lifecycle (LOCKED)

A fuzz finding is worthless if it can regress silently. The mandatory pipeline:

1. **Reproduce + minimize** (`cargo fuzz tmin`, or proptest's auto-shrink).
2. **Fix the code** — never relax the assertion (`goal.md` Gates: *"fix the code, never the
   assertion"*). A generic≠specialized divergence means a specialization guard is wrong; an `.aso`
   panic means a missing `verify`/`Reader` check; a round-trip mismatch means a serializer bug.
3. **Add a permanent regression test in the NORMAL suite** — not only in the fuzz corpus. The
   minimized program becomes:
   - a `#[test]`/`#[tokio::test]` case appended to `tests/vm_differential.rs` (for a differential
     divergence — it joins the hand-written three-way cases), **and/or**
   - a unit test next to the fixed code (for an `.aso`/serialize/parser crash), **and**
   - the raw minimized input is **also** committed to `fuzz/corpus/<target>/` so the per-PR job
     re-runs it forever (defense in depth: the corpus catches a re-mutation; the unit test documents
     the exact known bug).
4. **The fix PR includes the regression test** — green proves the fix; the committed corpus entry
   proves the fuzzer can no longer rediscover it from scratch.

This is how a transient fuzz finding becomes a **permanent guard** — the same discipline by which the
existing hand-written `sp3_*` recursion cases (`tests/vm_differential.rs:6874`+) and the
`decode_rejects_*` serialize cases (`serialize.rs:988`+) were born; FUZZ industrializes their
*discovery*.

## 5. Implementation surface

Per the campaign's "every subsystem this touches is a required deliverable" discipline. FUZZ adds **no
runtime/language surface** — only test/CI infrastructure and dev-dependencies.

- **`Cargo.toml`:**
  - Add `proptest` and `arbitrary` (with `derive`) to `ascript`'s `[dev-dependencies]` (proptest
    in-tree; `arbitrary` for the proptest-side generator). The isolated `fuzz/` crate carries
    `arbitrary` (+ `libfuzzer-sys`) as ordinary deps of *its own* manifest. Because the shared generator
    is `#[path]`-included (not a crate dep) and crate-gated `#[cfg(any(test, fuzzing))]` (§3.1), neither
    `arbitrary` nor the generator enters `ascript`'s **production** dependency graph, and a plain
    `cargo build` / `--no-default-features` *build* is unaffected (the property tests that touch
    feature-gated stdlib are themselves `#[cfg(feature=…)]`).
  - **No `[features]` change.** The differential generator targets the **core language** (it must run
    under `--no-default-features`, where the three-engine identity is the tightest); stdlib-specific
    properties are feature-gated within the test files.
- **A `fuzz/` crate (cargo-fuzz / libFuzzer):** a separate workspace member (own `Cargo.toml`, like
  `tree-sitter-ascript/`'s isolated workspace, so a plain `cargo build` does not pull libFuzzer/nightly).
  Targets:
  - `fuzz_targets/differential.rs` — `arbitrary` bytes → AST generator → render → run on
    `run_source_exit` + `vm_run_source` + `vm_run_source_generic` → assert the §2.1 oracle.
  - `fuzz_targets/aso_roundtrip.rs` — raw bytes → `Chunk::from_bytes` / `from_bytes_verified`; assert
    no panic; on `Ok`, assert `verify` + bounded runnable-accept (§2.2).
  - `fuzz_targets/worker_serialize.rs` — (a) arbitrary sendable `Value` → `encode`/`decode`
    round-trip; (b) raw bytes → `decode` rejection safety (§2.3).
  - `fuzz_targets/parser.rs` — raw bytes → `syntax::parser::parse` + legacy `parser::parse`; assert no
    panic + (on valid generator output) front-end agreement.
  - Shared generator at the single canonical path `src/fuzzgen/mod.rs` (the §3.1 AST generator), gated
    `#[cfg(any(test, fuzzing))]` so it never enters a normal/`--no-default-features` build. The
    libFuzzer targets `#[path = "../../src/fuzzgen/mod.rs"]`-include it (cargo-fuzz sets `--cfg
    fuzzing`); the in-tree proptest reaches the same module under `cfg(test)`. **One** generator
    definition, included by both — no `fuzz/`→`ascript` crate dependency, no drift (see §3.1).
- **`tests/` additions (in-tree, normal suite):**
  - `tests/property.rs` — the §2.4 proptest suite (numeric tower, GC, formatter idempotence, parser
    round-trip, front-end agreement). Seeded + deterministic; runs under both feature configs.
  - New `#[test]` regression cases land in `tests/vm_differential.rs` (differential finds) and beside
    the fixed code (`aso.rs`/`serialize.rs`/`parser.rs` unit finds), per §4.3.
- **Oracle entry points reused (no new API):** `run_source_exit`, `vm_run_source`,
  `vm_run_source_generic`, `build_file`, `run_aso_file`, `compile::compile_source`,
  `vm::aso::Chunk::from_bytes` (`aso.rs:453`), `vm::verify::Chunk::from_bytes_verified`
  (`verify.rs:782`, returns `Result<Chunk, FromBytesVerifiedError>`), `vm::verify::verify`,
  `worker::serialize::{encode, decode, check_sendable}`, `syntax::parser::parse` (+ legacy
  `parser::parse`). All already public or `#[doc(hidden)] pub`.
- **CI workflow files (`.github/workflows/`):**
  - Extend `ci.yml` (current: build/test/clippy in both feature configs, `:35`–`:48`) with a `fuzz`
    job — a per-PR time-boxed run of the cargo-fuzz targets from the committed corpus. (The proptest
    properties ride the existing `cargo test` steps, no new job needed.)
  - Add `fuzz-nightly.yml` — a `schedule:`-triggered deep run with corpus caching + crash-artifact
    upload + issue-on-crash. Mirrors the structure of `mirror-grammar.yml`.
- **Docs:** a short `CONTRIBUTING.md` section ("Found a fuzz crash? → minimize → fix code → add
  regression test + commit corpus entry") encoding §4.3, and a `CLAUDE.md` note that FUZZ is continuous
  infra (a syntax/numeric/`.aso` change must extend the generator/targets in the same PR, `goal.md`
  Gate 8). No `docs/` site `NAV` change (user-facing site is unaffected — this is contributor infra).

## 6. Determinism note

The generated programs feed a **byte-identical** differential, so their observable output must be a
deterministic function of the program text — otherwise a "divergence" is just two engines making
different-but-equally-valid nondeterministic choices, and the oracle is worthless. We reuse the
**workers' order-deterministic discipline** (workers spec §9: examples use `task.gather` order-
preservation + ordered consumption; `tests/vm_differential.rs` already compares only order-
deterministic worker output):

- **Deterministic-by-construction (preferred):** the generator simply does not emit nondeterministic
  observables — no `task.race`/completion-order printing, no unsorted `Map`/`Set` iteration to stdout,
  no clock/RNG/wall-time, no float formatting that differs across platforms (only the values the shared
  `Value` `Display` renders identically on every engine, which is the whole corpus's existing
  guarantee).
- **Compared-on-projection (fallback):** where a construct is useful but order-nondeterministic, the
  generator wraps the output in a deterministic projection (sort before print, gather instead of race)
  — the same trick the corpus uses.
- **SP9 seam:** if the generator ever emits RNG/clock-using stdlib, it does so **only** inside an SP9
  determinism context (Record/Replay, `VirtualClock`/`SeededRng`, `CLAUDE.md` SP9) and all three
  engines run under the **same** seed — so even those are reproducible. (NUM §7 confirms integer
  arithmetic adds no new clock/RNG seam, so the numeric-edge bias is deterministic for free.)
- **Fuzzer reproducibility:** every cargo-fuzz finding is a saved input file; proptest prints the seed.
  A failing case is replayable verbatim — a non-reproducible "flake" is itself a determinism bug to
  hunt, never to retry-until-green.

## 7. Testing the tests (the fuzzer must catch a known bug)

A fuzzer that finds nothing might be broken, not the code clean. Each target ships a **planted-bug
self-test** proving it catches a real divergence/crash, run in the normal suite:

- **Differential:** a "saboteur" engine wrapper (e.g. a one-line patched evaluator that computes
  `1+1==3`, or an off-by-one in one VM mode) — the generator + oracle must flag it within a bounded
  number of cases. **Pinned location:** the saboteur lives behind `#[cfg(test)]` in the differential
  harness module (`tests/property.rs` for the proptest host; a `cfg(test)`-only `enum SaboteurMode`
  threaded into the oracle wrapper) — it is **never** compiled into a normal/`fuzzing`/production build
  and **never** reachable by the libFuzzer target. The self-test **asserts the harness reports a
  divergence when the saboteur is enabled, and asserts no divergence when it is OFF** (the default), so
  a build that accidentally left it on fails loudly. (`#[cfg(test)]` + asserted-off — the saboteur can
  never silently corrupt a real run.)
- **`.aso`:** feed a curated **known-bad** byte set (a truncated proto, an out-of-range tag, a length
  claiming `u32::MAX`) and assert the target classifies them as `Err`, not `Ok`/panic — i.e. the
  harness's "panic ⇒ crash" detection actually fires (reuse/extend the `aso.rs` deser tests).
- **`.aso` huge-length-no-allocate (the P0 fix's PERMANENT guard, must-fix #1).** A dedicated
  `#[test]` next to the fixed reader feeds a minimal `.aso` whose collection length fields claim
  `0xFFFF_FFFF` while only a handful of payload bytes follow, and asserts `from_bytes` returns a clean
  `Err(AsoError::Truncated)` **without** a multi-GB allocation/abort — directly mirroring the worker
  serializer's `decode_huge_length_does_not_allocate` (`serialize.rs:1018`). This test is part of the
  §2.2 reader-clamp deliverable, is also committed as a seed under `fuzz/corpus/aso_roundtrip/`, and is
  the regression that proves the clamp fix stays applied — its presence (red before the fix, green
  after) is what makes the bug "found-and-fixed and proven-fixed," not merely patched.
- **Serialize:** plant a deliberately wrong round-trip (decode that drops a Map entry) behind a test
  flag; assert the round-trip property fails. The shipped `decode_rejects_*` tests
  (`serialize.rs:988`+) already double as the rejection-safety self-test.
- **Parser:** a string known to panic a deliberately-broken `parse` variant is caught; the real
  `parse` must survive it.

This makes "the fuzzer is green" mean "the fuzzer can fail," not "the fuzzer is asleep."

## 8. Scope & rejected alternatives

**In scope:** the grammar-aware differential program fuzzer (three-way + four-mode on saved cases); the
`.aso` deser/verify byte fuzzer (security-critical, gates BIN); the structured-clone round-trip +
rejection fuzzer; the in-tree proptest suite (numeric tower, GC, formatter idempotence, parser round-
trip, front-end agreement); shrinking; per-PR + nightly CI with corpus + crash→regression lifecycle;
planted-bug self-tests; determinism discipline.

**Out of scope (deferred, not silently dropped):**
- **Performance/throughput fuzzing** (finding slow inputs) — a separate concern from correctness; the
  `bench/` harness (workers spec §11.5) owns perf.
- **Fuzzing feature-gated network/DB stdlib end-to-end** (postgres/redis/http) — they need live
  servers and add nondeterminism; covered by their own integration tests, not the differential.
- **Concurrency-schedule fuzzing** (exploring task interleavings) — the async model's nondeterministic
  scheduling is an *architectural* M17 non-goal to make deterministic (`CLAUDE.md`); we fuzz
  order-deterministic projections only.

**Rejected:**
- **Relying only on the fixed example corpus.** The corpus tests the programs we thought of; the
  oracle's power is wasted without an unbounded program source. (This spec exists precisely to lift the
  corpus harness, `tests/vm_differential.rs:5316`, to a generator.)
- **Output-only fuzzing without an oracle.** Random bytes into the runtime find *crashes* but not
  *correctness divergences* — a program that panics identically in all three engines is not a bug. The
  differential oracle is the entire point; we keep it central and only use oracle-free fuzzing for the
  *byte-parsing* targets (where "no panic" IS the invariant).
- **One-shot fuzzing (a milestone, then done).** `goal.md` makes FUZZ **continuous infrastructure**, a
  pillar-1 standard every spec is held to (Gate 8) — not a box to tick. Hence the per-PR + nightly CI
  wiring and the crash→permanent-regression lifecycle.
- **A single tool (proptest-only OR cargo-fuzz-only).** **Locked: BOTH.** proptest gives in-tree,
  deterministic, auto-shrinking *structured property* tests that run in the normal `cargo test`
  (numeric/GC/formatter invariants); cargo-fuzz/libFuzzer gives *coverage-guided byte* fuzzing of the
  parsers + `.aso` (where coverage feedback over the `read_*` recursion is irreplaceable). The shared
  `arbitrary`-based generator lets one AST generator feed both, so they do not drift.
- **A hand-written random-program generator divorced from `arbitrary`.** Coupling generation to
  `arbitrary`'s unstructured source is what lets libFuzzer's coverage feedback steer *program*
  generation — a bespoke RNG generator forfeits coverage guidance.

## 9. Honesty: coverage limits (no silent caps)

Fuzzing finds bugs; it does not prove their absence. Stated per target:

- **Differential:** only finds divergences on programs the generator can **produce** and that are
  **deterministic**. Constructs outside the generated grammar subset (initially: complex async timing,
  network stdlib, exotic stdlib corners) are **not** covered by the differential and stay covered by
  the hand-written corpus. The generated-grammar coverage is **explicitly tracked** and grown per
  syntax spec (NUM/ADT/IFACE each widen it) — a construct's absence from the generator is a documented
  gap, never a silent one.
- **`.aso` runnable-accept** is **bounded** (step/time budget) — it cannot prove every accepted chunk
  halts (halting problem); it proves accepted chunks don't *immediately* crash the VM within the
  budget. A non-terminating accepted chunk is a separate (verifier-completeness) concern, documented,
  not silently capped.
- **GC property:** asserts no leak/double-free over the **generated graph shapes**; it cannot prove the
  collector correct for all heaps. It interacts with `gcmodule`'s thread-local heap — the property runs
  on the main test thread's heap and forces collection explicitly; it does not (yet) fuzz cross-worker-
  isolate GC interaction (each isolate has its own heap, workers spec — a documented future extension).
- **proptest** explores a sampled, shrink-guided slice of the input space per run (default cases =
  256, raised for the numeric properties); it is not exhaustive. The seed is printed for replay.
- **cargo-fuzz** coverage is only as good as the corpus + time budget; the per-PR run is intentionally
  short (a smoke re-run of known-interesting inputs), the nightly is the real campaign. Neither is a
  proof.

These limits are **documented in the harness and CONTRIBUTING**, never papered over — consistent with
`goal.md` Gate 6 ("no placeholders / silent deferrals").

## 10. Grounding (verified sources)

- **Differential compiler/runtime testing & grammar-aware generation:** CSmith (Yang et al., PLDI'11 —
  randomized differential testing of C compilers via a generator emitting *valid* programs to make
  miscompiles, not just crashes, detectable); the broader differential-testing tradition (McKeeman,
  "Differential Testing for Software", 1998).
- **Grammar-aware fuzzing of language runtimes:** jsfunfuzz / DOMfuzz (Mozilla — generating valid-ish
  JS to find SpiderMonkey engine bugs); LangFuzz; the general "generate from the grammar, bias toward
  edges" technique.
- **Coverage-guided byte fuzzing:** libFuzzer (LLVM) + `cargo-fuzz`; AFL/AFL++ (coverage-guided
  mutation); their use for hardening *deserializers/parsers* against untrusted input is the textbook
  application for the `.aso`/serialize targets.
- **Structured property testing + shrinking:** QuickCheck (Claessen & Hughes, ICFP'00 — properties +
  automatic shrinking); `proptest` (the Rust port; integrated shrinking) — the model for the in-tree
  numeric/GC/formatter/round-trip suite.
- **`arbitrary` as the shared structured-input source** bridging proptest strategies and libFuzzer
  byte streams (Rust `arbitrary` crate; cargo-fuzz's `arbitrary` integration).
- **Round-trip / serialization invariants:** the standard `decode(encode(x)) == x` property; WHATWG
  structured-clone semantics (the workers spec's serializer basis) as the equality oracle.
