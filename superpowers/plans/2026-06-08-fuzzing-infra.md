# FUZZ — Differential Fuzzing & Property Testing Infrastructure — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`. This is **continuous
> infrastructure** — the operational form of the "No bugs" pillar (`goal-brief.md` pillar 1). It is
> **stood up alongside NUM** and runs in CI thereafter; it **gates BIN**.

**Spec:** `superpowers/specs/2026-06-08-fuzzing-infra-design.md`. **Branch:** `feat/fuzzing-infra` off
`main`. **Depends on:** nothing structurally — stands up *alongside* NUM (the `Value::Int`-referencing
properties are written **inside the NUM PR**, not here; see Task 9). **Breaking:** no — adds zero language
surface, no runtime behavior change, only test/CI infra + dev-dependencies. Must not perturb a plain
`cargo build` or `--no-default-features` *build*.

**Architecture:** turn the existing **fixed-corpus** three-way differential
(`assert_three_way_matches` `tests/vm_differential.rs:5284`; whole-corpus net `:5316`) into a
**continuous generator-driven oracle**. One grammar-aware `arbitrary`-driven AST generator at
`src/fuzzgen/` (crate-gated `#[cfg(any(test, fuzzing))]`) is shared — *by `#[path]` include, never a
crate dep* — between (a) in-tree `proptest` suites (`tests/property.rs`, normal `cargo test`) and (b) a
separate `fuzz/` cargo-fuzz workspace member. Four fuzz targets: differential program, `.aso`
deser+verify (security-critical, **owns the permanent guard for the P0 reader-clamp**), worker
structured-clone round-trip, and source parser. CI runs per-PR time-boxed + a nightly deep campaign; the
quantified sustained-clean bar (§4.1) is the literal BIN handshake.

**Tech stack:** Rust; `proptest` + `arbitrary` (dev-deps only); `cargo-fuzz`/`libfuzzer-sys` (isolated
`fuzz/` crate); reuse the public oracle seams `run_source_exit`/`vm_run_source`/`vm_run_source_generic`/
`build_file`/`run_aso_file`; `vm::aso`/`vm::verify`/`worker::serialize`/`syntax::parser`/`parser`+`lexer`;
GitHub Actions.

---

## Shared API Contract (pinned to current code — verified 2026-06-08)

**Oracle entry points (all `pub`, several `#[doc(hidden)] pub`):** `run_source` `lib.rs:175`;
`run_source_exit` `lib.rs:180` (`Result<(String, Option<i32>), AsError>`); `vm_run_source` `lib.rs:332`;
`vm_run_source_generic` `lib.rs:345`; `build_file` `lib.rs:356` (`.as` → `.aso` path); `run_aso_file`
`lib.rs:389`; `run_on_worker_stack` `lib.rs:53` (the 512 MB worker-stack driver — fuzz harnesses that run
programs must wrap on it). Note `run_source_exit`/`vm_run_source*` are `async` (current-thread tokio +
`LocalSet`; the runtime is `!Send`).

**`.aso` reader (the security target + P0 guard):** `Chunk::from_bytes` `aso.rs:453`
(`Result<Chunk, AsoError>`); `ASO_FORMAT_VERSION: u32 = 18` `aso.rs:105`; `Reader` struct `aso.rs:345`,
`Reader::take` `aso.rs:354` (bounds-checked), **NO `remaining()` method exists in `aso.rs` today**;
`read_chunk` `aso.rs:565`, `read_value` `aso.rs:689`, `read_proto` `aso.rs:760`, `read_type`
`aso.rs:903`, `read_expr` `aso.rs:1168`/`read_expr_kind` `:1180`, `read_class_proto` `aso.rs:1384`.
`from_bytes_verified` lives in **`verify.rs:782`** (NOT `aso.rs`) and returns
`Result<Chunk, FromBytesVerifiedError>` where `FromBytesVerifiedError` is the wrapper enum
`{Decode(AsoError), Verify(VerifyError)}` `verify.rs:792`; `verify` free fn `verify.rs:331`; `VerifyError`
`verify.rs:39`.

**Worker airlock:** `encode` `serialize.rs:360` (`Result<Vec<u8>, SendError>`); `decode` `serialize.rs:517`
(`decode(bytes, interp) -> Result<Value, SendError>`); `check_sendable` `serialize.rs:103`; `SendError`
`serialize.rs:34`. **The clamp pattern to mirror:** `Vec::with_capacity(len.min(r.remaining()))`
`serialize.rs:564`; its `remaining()` `serialize.rs:306`. Existing hardening self-tests (the seeds):
`decode_rejects_truncated_buffer` `:988`, `decode_rejects_unknown_tag` `:1004`,
`decode_rejects_dangling_ref` `:1010`, `decode_huge_length_does_not_allocate` `:1018`.

**Parsers:** CST `syntax::parser::parse(src: &str) -> Parse` `syntax/parser.rs:170` (degrades to error
nodes via `p.error` `:159` + `p.bump` `:106` — never panics by contract). Legacy
`parser::parse(tokens: &[Token]) -> Result<Vec<Stmt>, AsError>` `parser.rs:8` — takes **tokens**, so the
parser target must `lexer::lex(src) -> Result<Vec<Token>, AsError>` `lexer.rs:82` first. Front-end
conformance helpers (the agreement seed): `accepts`/`cst_accepts`/`both_accept`
`tests/frontend_conformance.rs:21`/`:28`/`:43`.

**Differential corpus harness (to lift to a generator):** `assert_vm_matches_treewalker(expr_src)`
`vm_differential.rs:25` (single-expression); `assert_three_way_matches(src)` `:5284` (the projection
normalizer — stdout/exit/panic+span); whole-corpus net `:5316`; hand-written SP3 recursion cases
`sp3_assert_three_way_identical` `:6845` and `sp3_*` `:6875`+ (the boundary cases the deep-nesting bias
extends).

**GC:** `gc::collect() -> usize` `gc.rs:107` (returns reclaimed); `gcmodule::count_thread_tracked()` is
the live-count probe used by the existing no-leak tests (`gc.rs:778`–`789` before/drop/after delta is the
exact property shape to generalize); `Value::trace` mirrors cycle-capable containers (`CLAUDE.md` Values).

**Workspace isolation (the mechanism a plain build must not break):** root `Cargo.toml` has **no
`[workspace]`**; `tree-sitter-ascript/Cargo.toml:18` declares its own empty `[workspace]` so `cargo build`
does not absorb it. `fuzz/` mirrors this pattern exactly. Root `[dev-dependencies]` `Cargo.toml:102`;
`[features]` default set `Cargo.toml:106`. CI: `.github/workflows/ci.yml` (`test` job `:18`, build/test ×2
configs/clippy ×2 configs `:35`–`:48`; `on: pull_request` + `push:[main]` `:3`); `mirror-grammar.yml` is
the schedule-workflow template to mirror for nightly.

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs
  (`--all-targets` and `--no-default-features --all-targets`).
- **Invariant: a plain `cargo build` and `cargo build --no-default-features` pull in NEITHER `proptest`,
  `arbitrary`, `libfuzzer-sys`, nor `src/fuzzgen/`** (verify with `cargo tree` after each task that adds
  deps). The generator is `#[cfg(any(test, fuzzing))]`-gated; `arbitrary` is dev-dep-only on `ascript`.
- Generated programs are **deterministic-by-construction** (no clock/RNG/unsorted-iteration/race output;
  spec §6) — otherwise the differential is meaningless. Compare on the `assert_three_way_matches`
  projection (stdout/exit/panic+span), never raw internal state.
- Fix the code, never the assertion (`goal-brief.md` Gates). A `generic≠specialized` divergence is a
  wrong specialization guard; an `.aso` panic is a missing `Reader`/`verify` check.
- No new production deps, no `examples/`/grammar/NAV changes. Contributor docs go in `CONTRIBUTING.md`.

---

## Task 1 — P0 `.aso` reader-clamp fix (the FUZZ campaign's first catch; gates BIN)
**Files:** `src/vm/aso.rs`. **Tests:** `src/vm/aso.rs` `#[cfg(test)]`.

> This is the live abort/OOM bug (`from_bytes` allocates unboundedly on crafted input). FUZZ **owns**
> the permanent guard. Mirrors the P0 plan (`superpowers/plans/2026-06-08-p0-aso-reader-clamp.md`); if
> that standalone PR already merged, this task is a verification-only no-op (confirm the clamp + guard are
> present and adopt the seed) — DO NOT duplicate the change.

- [ ] **Failing test** `reader_huge_length_does_not_allocate` (model on `serialize.rs:1018`): build a
  buffer with a valid header (`w.u32(ASO_FORMAT_VERSION)`) then a const-pool / value length field
  claiming `u32::MAX` over a short payload; assert `Chunk::from_bytes(&bytes)` returns
  `Err(AsoError::Truncated)` (or another clean `Err`) **without** a multi-GB allocation/abort. Pick the
  earliest length-driven site so the test is minimal.
- [ ] Add `Reader::remaining(&self) -> usize` to `aso.rs` (it has none — return
  `self.bytes.len().saturating_sub(self.pos)`, matching `Reader::take` `aso.rs:354`'s field names).
- [ ] Grep every `Vec::with_capacity(n)` / `IndexMap::with_capacity(n)` / `*.reserve(n)` in `read_chunk`
  (`:565`+) and the recursive `read_value`/`read_proto`/`read_type`/`read_expr`/`read_class_proto`
  (`:689`/`:760`/`:903`/`:1168`/`:1384` and callees) where `n` derives from a `r.u32()?`/length read; clamp
  each to `n.min(r.remaining())`. Each element is ≥1 byte so the clamp never under-reserves a valid chunk;
  the per-element decode loop still errors cleanly on a short read. No format change, **no
  `ASO_FORMAT_VERSION` bump**.
- [ ] Green both configs; clippy. **Independent review:** runs the test, greps for any remaining unclamped
  `with_capacity`/`reserve`, confirms `ASO_FORMAT_VERSION` unchanged. The huge-length test is the
  **permanent regression guard** (§7) and becomes a committed `fuzz/corpus/aso_roundtrip/` seed in Task 5.
  Commit.

## Task 2 — `fuzz/` isolated workspace skeleton + cargo-fuzz scaffold
**Files (new):** `fuzz/Cargo.toml`, `fuzz/.gitignore`, `fuzz/fuzz_targets/.gitkeep`. **Tests:** build-only.

- [ ] Create `fuzz/` as a **separate workspace member with its own empty `[workspace]`** (mirror
  `tree-sitter-ascript/Cargo.toml:18`) so a plain `cargo build` at the repo root does NOT pull in
  `libfuzzer-sys`/nightly. Its `[dependencies]`: `libfuzzer-sys`, `arbitrary` (with `derive`), and
  `ascript` (`path = ".."`, `default-features = false` for the differential/parser targets; the targets
  that need stdlib enable features explicitly). `[package.metadata]` `cargo-fuzz = true`.
- [ ] **No `fuzz/`→shared-generator crate dependency.** The generator is `#[path]`-included per-target
  (Task 4), not a crate dep — so the `ascript` dep here is only for the public oracle seams.
- [ ] Confirm `cargo build` and `cargo build --no-default-features` at the repo root are byte-for-byte
  unaffected (`cargo tree` shows no `libfuzzer-sys`/`arbitrary` in the root graph). Add a one-line
  `CONTRIBUTING.md` pointer that `fuzz/` is an isolated nightly-only member (full prose in Task 8).
- [ ] Review (confirms isolation); commit. *(No fuzz targets yet — they land in Tasks 4–7.)*

## Task 3 — `proptest`/`arbitrary` dev-deps + `tests/property.rs` skeleton + GC no-leak property
**Files:** `Cargo.toml` (`[dev-dependencies]`). **Tests (new):** `tests/property.rs`.

- [ ] Add `proptest` and `arbitrary` (with `"derive"`) to `ascript`'s `[dev-dependencies]`
  (`Cargo.toml:102`). Verify they do NOT enter the production graph (`cargo tree -e normal` clean).
- [ ] Create `tests/property.rs` — the in-tree proptest host (deterministic, seeded; runs in the normal
  `cargo test` under BOTH feature configs, not the time-boxed fuzz job).
- [ ] **GC no-leak / no-double-free property** (the first property, generator-independent): a `proptest`
  `Strategy` building a random object/array/map graph with arbitrary cycles (`a.push(a)`-style), drop the
  roots, call `gc::collect()` (`gc.rs:107`), assert live count (`gcmodule::count_thread_tracked()`)
  returns to the pre-build baseline (no leak) AND a second `collect()` reclaims 0 (no double-free / no
  resurrection). Mirror the existing before/drop/after delta shape at `gc.rs:778`–`789`. Document the §9
  limit (single main-thread heap, no cross-isolate GC fuzzing yet).
- [ ] Green both configs; clippy. Review (confirms no production-graph leakage; seed-replay works). Commit.

## Task 4 — Grammar-aware AST generator at `src/fuzzgen/` (the core asset)
**Files (new):** `src/fuzzgen/mod.rs` (+ submodules); `src/lib.rs` (one gated `mod fuzzgen;`). **Tests:**
inline `#[cfg(test)]` smoke tests in `src/fuzzgen/`.

- [ ] Add `#[cfg(any(test, fuzzing))] mod fuzzgen;` to `src/lib.rs` (crate-level gate per spec §3.1) — so
  it compiles into `ascript` ONLY for `cargo test` and never in a normal/`--no-default-features` build.
- [ ] Implement an **`arbitrary::Arbitrary`-driven, recursion-budgeted** generator producing well-formed
  **legacy `ast`** trees (the types both `Display`/`fmt` and `parser`/`syntax::parser` round-trip
  against). A depth/size budget decremented per recursion guarantees termination. Two granularities:
  an **expression** generator (feeds `assert_vm_matches_treewalker`-style checks, `vm_differential.rs:25`)
  and a **program** generator (fns/classes/control-flow → full differential).
- [ ] **Scope-correct by construction:** thread a symbol environment so every emitted identifier is in
  scope, every `const` is never reassigned, calls match arity, and `break`/`continue`/`return`/`yield`
  appear only where legal — maximizing the fraction that compiles (reaches all three engines).
- [ ] **Deterministic-by-construction:** never emit `task.race`, unsorted `Map`/`Set` iteration to stdout,
  `uuid.v4`/`time.now`, or platform-divergent float formatting (spec §6). Render the AST to source via the
  existing `Display`/`fmt` path.
- [ ] **Edge-biased distribution** (the value of a custom generator): weight toward deep nesting near
  `MAX_CALL_DEPTH`/`EXPR_NEST_LIMIT` (extends the `sp3_*` boundary cases `vm_differential.rs:6875`+),
  closures + capture-by-value (per-iteration loop freshness), `match` ranges/guards/Option-C
  bind-vs-compare. (Numeric-edge bias — literals near `i64::MIN/MAX`, `2^53±1`, mixed int/float, `>>`-split
  generics — is added in the **NUM PR** when `Value::Int` exists, Task 9; the FUZZ scaffolding generator
  targets only the pre-NUM `Value::Number` surface.)
- [ ] Inline `#[cfg(test)]` smoke test: generate N programs from fixed `arbitrary` byte seeds, assert each
  parses + runs without the *generator itself* panicking. Green both configs; clippy. Review (confirms the
  generator never emits out-of-scope identifiers / nondeterministic output; confirms the gate keeps it out
  of a plain build via `cargo tree`). Commit.

## Task 5 — `.aso` deser+verify byte-fuzz target + permanent P0 guard proven
**Files (new):** `fuzz/fuzz_targets/aso_roundtrip.rs`; `fuzz/corpus/aso_roundtrip/` (seeds). **Tests:**
extend `src/vm/aso.rs` `#[cfg(test)]`.

- [ ] `aso_roundtrip.rs`: raw libFuzzer bytes → `Chunk::from_bytes` (`aso.rs:453`) and
  `Chunk::from_bytes_verified` (**`verify.rs:782`**, returns `Result<Chunk, FromBytesVerifiedError>`).
  **Invariant:** always `Ok` / `Err(AsoError)` / `Err(FromBytesVerifiedError)` — never panic, OOB,
  unbounded-alloc, hang, or UB.
- [ ] **Runnable-accept (bounded):** when `from_bytes_verified` returns `Ok`, run the chunk on the VM under
  a step/time budget (wrap on `run_on_worker_stack` `lib.rs:53`) and assert it terminates with a
  `RunOutcome`/`Control`, not a host panic — a *verified* chunk that crashes the VM is a `verify.rs` gap
  (a security finding). Document the halting-problem bound (§9).
- [ ] **Seed corpus = real `.aso`** built by `build_file` (`lib.rs:356`) over `examples/**` (the
  structure-aware mode reaches the deep `read_*` arms where the clamp-class bugs live), plus the Task 1
  huge-length buffer committed as a permanent seed.
- [ ] **`.aso` planted-bug self-test (in the normal suite, §7):** a curated known-bad byte set (truncated
  proto, out-of-range tag, `u32::MAX` length) asserted to classify as `Err`, proving the harness's
  "panic ⇒ crash" detection actually fires. The Task 1 `reader_huge_length_does_not_allocate` is the P0
  fix's permanent guard (red before the clamp, green after).
- [ ] `cargo +nightly fuzz run aso_roundtrip -- -runs=…` smoke-runs locally clean; both configs green;
  clippy. Review (runs the fuzzer briefly, confirms the version-reject + a deep mutated input both behave;
  confirms the seed corpus carries `ASO_FORMAT_VERSION=18`). Commit.

## Task 6 — Worker structured-clone round-trip + rejection-safety target
**Files (new):** `fuzz/fuzz_targets/worker_serialize.rs`; `fuzz/corpus/worker_serialize/`. **Tests:**
extend `src/worker/serialize.rs` `#[cfg(test)]`.

- [ ] `worker_serialize.rs`, two sub-modes: **(a) round-trip** — an `arbitrary` *sendable* `Value` graph
  (incl. cycles), assert `decode(encode(v), interp)` (`serialize.rs:360`/`:517`) is value-equal to `v`
  under AScript equality incl. cycle topology (shared sub-objects stay shared) and Map-key canonicalization
  (−0.0→+0.0, NaN). **(b) rejection-safety** — raw/mutated bytes → `decode` returns `Ok`/`Err(SendError)`,
  never panic / over-alloc (generalizes `decode_huge_length_does_not_allocate` `:1018`).
- [ ] **Sendability-honesty invariant:** `encode(v)` succeeds **iff** `check_sendable(v)` (`:103`) is
  `Ok` — an un-sendable kind (`Function`/`Native`/`Future`/`Generator`) yields a `SendError` with a field
  path, never a panic and never silent loss. (The `Value::Int` Map-key sub-cases land in the **NUM PR**,
  Task 9 — the `Float` tag = former `Number` tag.)
- [ ] Seed corpus from the existing `decode_rejects_*` byte buffers (`serialize.rs:988`+). Planted-bug
  self-test (§7): the shipped `decode_rejects_*` tests already double as the rejection-safety self-test;
  add a `#[cfg(test)]`-flagged deliberately-lossy decode and assert the round-trip property catches it.
- [ ] Smoke-run clean; both configs green; clippy. Review. Commit.

## Task 7 — Differential program target + source-parser target + planted-bug saboteur
**Files (new):** `fuzz/fuzz_targets/differential.rs`, `fuzz/fuzz_targets/parser.rs`. **Tests:**
`tests/property.rs` (proptest-hosted differential + formatter/round-trip/front-end properties + the
saboteur self-test).

- [ ] `differential.rs`: `arbitrary` bytes → `src/fuzzgen` generator (`#[path =
  "../../src/fuzzgen/mod.rs"] mod fuzzgen;`, built with cargo-fuzz's `--cfg fuzzing`) → render → run on
  `run_source_exit` (`lib.rs:180`) + `vm_run_source` (`:332`) + `vm_run_source_generic` (`:345`),
  wrapped on `run_on_worker_stack`. **Oracle:** the three agree on the `assert_three_way_matches`
  projection (`vm_differential.rs:5284`). The four-mode check (adding `build_file`→`run_aso_file`,
  `:356`/`:389`) runs only on **saved crashes + examples** (slower), per spec §2.1.
- [ ] `parser.rs`: raw bytes → `syntax::parser::parse(src)` (`syntax/parser.rs:170`) and (after
  `lexer::lex(src)` `lexer.rs:82`) legacy `parser::parse(tokens)` (`parser.rs:8`) — assert neither panics;
  on generator-produced valid source, assert front-end **agreement** (accept/reject + structure, excluding
  the documented 1-column caret offset, `CLAUDE.md` SP1).
- [ ] **In-tree proptest properties** in `tests/property.rs` reusing `src/fuzzgen` under `cfg(test)`:
  **formatter idempotence** (`fmt(fmt(x))==fmt(x)` + `parse(x)`≡`parse(fmt(x))`); **parser round-trip**
  (`parse(print(ast))`≡`ast`); **front-end agreement** (generalizes `frontend_conformance.rs:43`).
- [ ] **Planted-bug saboteur self-test (§7, pinned):** a `#[cfg(test)]`-only `enum SaboteurMode` threaded
  into the proptest-hosted differential wrapper (a one-line patched evaluator, e.g. `1+1==3`) —
  **never** compiled into a `fuzzing`/production build, **never** reachable by the libFuzzer target. The
  self-test **asserts a divergence IS reported when the saboteur is ON and NONE when OFF (the default)** —
  so a build that left it on fails loudly. This makes "the fuzzer is green" mean "the fuzzer can fail."
- [ ] Smoke-run both targets clean; both configs green (proptest properties run in the normal suite);
  clippy. Review (verifies the saboteur is `#[cfg(test)]`+asserted-off and unreachable from `fuzz/`;
  runs the differential briefly). Commit.

## Task 8 — CI wiring (per-PR time-boxed + nightly deep) + the quantified BIN gate + corpus discipline
**Files:** `.github/workflows/ci.yml` (extend); `.github/workflows/fuzz-nightly.yml` (new);
`fuzz/corpus/**` (git-tracked seeds); `CONTRIBUTING.md` (new section); `CLAUDE.md` (note).

- [ ] **Per-PR (`ci.yml`):** add a `fuzz` job (nightly-toolchain + `cargo-fuzz`) that runs each target for
  a **short fixed budget** (60–120 s/target) from the committed `fuzz/corpus/` (re-exercises every
  historical crash). The proptest properties already ride the existing `cargo test` steps (`ci.yml:38`/
  `:42`) — no new job for them.
- [ ] **Nightly (`fuzz-nightly.yml`, `schedule:`-triggered, mirror `mirror-grammar.yml`):** run each target
  for minutes–hours; persist the grown corpus via `actions/cache` keyed per-target **AND on
  `ASO_FORMAT_VERSION`** (so a bump invalidates the `aso_roundtrip` cache and forces a fresh seed build,
  spec §4.2); run a coverage pass over the `read_*` family; on crash, minimize (`cargo fuzz tmin`),
  upload the reproducer artifact, and open/update a tracking issue.
- [ ] **The BIN gate (the literal handshake, spec §4.1):** encode the sustained-clean bar — **≥ 7
  consecutive ≥ 4 h nightly `aso_roundtrip` runs, zero crashes, streak reset on any `read_*`/`verify.rs`
  edit; ≥ 90 % line coverage of the `read_*` family in `aso.rs`; Task-1 clamp merged + its
  huge-length self-test green.** Surface these as a checked status (a nightly summary artifact /
  badge BIN's plan cites — not a loose "green in CI").
- [ ] **`.aso` seed regeneration on `ASO_FORMAT_VERSION` bump (spec §4.2/must-fix #5):** add a CI check
  that fails a PR bumping `ASO_FORMAT_VERSION` (`aso.rs:105`) unless `fuzz/corpus/aso_roundtrip/` is
  regenerated from `examples/**` in the same PR (stale-version seeds only ever hit the version-reject path,
  collapsing the coverage floor). Add this to the `CLAUDE.md` `.aso`-versioning checklist.
- [ ] **`CONTRIBUTING.md` section** "Fuzzing & property tests" encoding the §4.3 crash→regression
  lifecycle (reproduce → `tmin` → **fix the code, never the assertion** → add a permanent regression test
  in the NORMAL suite (a `#[test]` in `vm_differential.rs` for a differential find, or a unit test beside
  the fixed `aso.rs`/`serialize.rs`/`parser.rs`) → **also** commit the minimized input to
  `fuzz/corpus/<target>/`) + the §9 honesty limits (fuzzing finds bugs, never proves absence; per-target
  coverage gaps). **No `docs/` NAV change** (contributor infra, not the user-facing site).
- [ ] **`CLAUDE.md` note:** FUZZ is continuous infra — a syntax/numeric/`.aso` change must extend the
  generator/targets in the SAME PR (`goal-brief.md` Gate 8). Verify the per-PR `fuzz` job runs green on
  this branch; nightly is dry-run-validated (correct `schedule:` + cache keys). Review (runs the workflows
  via `act`/manual dispatch or asserts YAML validity + the `ASO_FORMAT_VERSION` tripwire fires on a
  synthetic bump). Commit.

## Task 9 — (lands in the NUM PR, NOT here) numeric-tower properties + `Int` round-trip sub-cases
**Owner:** the NUM plan (`superpowers/plans/2026-06-08-numeric-model.md` Task 15) — recorded here so the
sequencing is explicit and not silently dropped.

- [ ] These reference `Value::Int(i64)`/`Value::Float(f64)`/`MapKey::Int`, which **do not exist until NUM
  merges**. Written + committed **inside the NUM PR**, reusing this plan's `tests/property.rs` host +
  `src/fuzzgen` generator: exact int/float comparison across the 2^53 boundary; Map-key consistency
  (`(a==b)⟺same key`, NaN carved out); the §3.2 overflow/wrap/div0 table; division/remainder identities;
  the worker `encode∘decode preserves Int` + integral-float→Int key-fold round-trip sub-cases (§2.3).
- [ ] The NUM PR also adds the numeric-edge bias to `src/fuzzgen` (Task 4 left it pre-NUM). The FUZZ
  scaffolding (Tasks 1–8) must merge **before or alongside** NUM so this host/generator exists; NUM
  rebases onto whichever lands first. *(No work in THIS plan — a forward reference for the cross-spec
  handshake `goal-brief.md` reconciliation.)*

## Done when
Tasks 1–8 each checked behind an independent review. The P0 clamp is fixed + permanently guarded (Task 1);
one `src/fuzzgen` generator is shared by proptest + `fuzz/` via `#[path]` include with **no
`fuzz/`→in-tree crate dep** and `arbitrary`/`proptest`/`libfuzzer-sys` absent from the production graph
(verified `cargo tree`); a plain `cargo build` and `--no-default-features` build are unaffected. All four
fuzz targets (differential / `.aso` / worker-serialize / parser) build and smoke-run clean; the in-tree
proptest properties (GC, formatter idempotence, parser round-trip, front-end agreement) run green in BOTH
feature configs; every target ships a planted-bug self-test asserted-off. CI runs the per-PR time-boxed
job + the nightly deep campaign with corpus caching keyed on `ASO_FORMAT_VERSION`; the quantified BIN gate
(7×4 h clean + 90 % `read_*` coverage + clamp-merged) is encoded and surfaced. `CONTRIBUTING.md` documents
the crash→regression lifecycle + honesty limits; `CLAUDE.md` records FUZZ as continuous infra. Clippy +
tests green both configs. Merge `--no-ff` to `main`. *(NUM-dependent numeric properties land in the NUM
PR, Task 9.)*
