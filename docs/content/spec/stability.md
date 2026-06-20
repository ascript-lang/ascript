# Stability & the road to 1.0

This final chapter governs **change**: how the language is versioned, which parts of
the surface carry a stability promise, how breaking changes are handled, and the
process by which the language evolves. The preceding chapters say *what AScript is
today*; this chapter says *what may change tomorrow, and how*.

Where a chapter describes behaviour, the [conformance suite](conformance) is the
executable judge. Where a chapter describes a **policy** — as this one does — the
owner is the judge: the lists below are explicitly **owner-editable** and are
maintained as the language moves toward 1.0.

## Language version

**The language version is the crate version.** One binary is the one implementation;
a separate language-version counter would immediately drift, so AScript declares its
version as `Cargo.toml`'s `version` field — **0.6** at the time this chapter was
written. The spec set as a whole is versioned with the language, and each chapter
carries the implementation it was verified against.

AScript is **pre-1.0**. Pre-1.0, a breaking change to the **STABLE** surface (below)
is **permitted**, but only with all three of:

1. a **minor** version bump (patch releases are non-breaking);
2. **migration notes** recorded in the release / roadmap record; and
3. the **corpus migrated** in the same change — `examples/**` is updated to the new
   behaviour, never deleted, so the diff of the corpus *is* the first draft of the
   migration guide.

A release that changes spec'd behaviour updates the affected chapter in the **same**
PR. The grammar half of that obligation is enforced mechanically (see
[Conformance](#conformance)); the semantics half is a reviewer duty backed by the
`CLAUDE.md` "Touching syntax" checklist.

## Stability tiers

Every part of the surface sits in exactly one of three tiers. **Each list is
owner-editable** and is the live record, not a frozen promise.

### STABLE — the spec'd surface *(owner-editable)*

Everything chapters 2–13 of this specification state normatively, **plus** the
standard-library module APIs documented by the reference under
`docs/content/stdlib/` (per the [standard-library chapter](stdlib)'s rules). This is
the contract programs may rely on.

Breaking a STABLE behaviour requires an RFC-lite (see [Changing the
language](#changing-the-language)) **plus** the version bump and migration notes
pre-1.0; **post-1.0 it requires a major version.**

The `--tree-walker` engine flag and **four-mode identity** are STABLE *as a
guarantee* — the differential oracle is permanent — even though the engines'
internals are INTERNAL.

### EXPERIMENTAL — listed, may change without an RFC *(owner-editable)*

The following surfaces are shipped but **explicitly provisional**: they may change,
be renamed, or be removed **without** an RFC. The list is derived from the
implementation's recorded deferral notes (CLAUDE.md "Current deferrals" + the design
record §5); nothing here is speculative.

- **`http3`** — the opt-in Cargo feature for HTTP/3. Upstream (`reqwest`'s HTTP/3)
  is unstable by its own deferral note and additionally needs
  `RUSTFLAGS="--cfg reqwest_unstable"`; the feature surface may change with it.
- **The debugger (DAP) surface beyond what shipped** — the DBG v1 deferrals:
  transient single-line stepping and conditional breakpoints / logpoints (today's
  stepping is resume-to-next-breakpoint), and the **profiler output formats**
  (speedscope JSON / collapsed folded-stacks file shapes).
- **Record / replay as a user-facing feature** — the determinism seams in
  `src/det.rs` are shipped but **INERT** by default. The user-facing record/replay
  surface is owned by a future REPLAY effort; until it lands, any exposed knob is
  experimental.
- **The advisory lint-code inventory and `[lint]` keys** — the **blocking soundness
  behaviour** (the [types chapter](types)'s `type-mismatch`-on-an-annotated-slot
  Error) is STABLE; but the *set* of advisory lint codes, their **names**, and the
  `ascript.toml [lint]` keys that tune them may grow or be renamed.
- **`std/ai` and `std/telemetry` wire formats** — `std/ai` tracks fast-moving
  upstream provider APIs; the telemetry wire formats track evolving exporters.
- **The `ascript doc` output format and the LSP capability set** — both are on the
  DX track and still growing (per the SIG signature-table work).
- **Implementation-defined subsets, called out as such** — `std/intl` (an ICU data
  subset), `std/tui` (a crossterm feature subset), and best-effort HTTP response
  trailers. The [introduction chapter](intro) classifies these as
  *implementation-defined*: chosen and documented by the implementation, free to
  change.

### INTERNAL — versioned or private, no stability promise *(owner-editable)*

No program should depend on any of these; they exist to serve the STABLE surface and
may change at any time.

- **The `.aso` bytecode artifact** — explicitly **versioned-but-internal**. The
  constant `ASO_FORMAT_VERSION` (`src/vm/aso.rs`) guards it: an `.aso` file is valid
  **only for the binary version that produced it**; across versions, rebuild from
  source. The format is not a distribution or interchange format.
- **The opcode set and bytecode layout**, and the **worker structured-clone wire
  tags** that cross the serializer airlock.
- **The shape / inline-cache machinery, adaptive arithmetic, and the `Vm.instrument`
  debugger seam.**
- **Internal environment variables and diagnostic kill switches** —
  `ASCRIPT_NO_SPECIALIZE` (and the sibling `ASCRIPT_NO_SYNC_LANE` /
  `ASCRIPT_NO_CALL_FAST` / `ASCRIPT_NO_DECODE` knobs) exist for diagnostics and the
  differential gate; they are not a configuration surface.
- **The Rust API of the `ascript` crate, *except* `ascript::embed`.** With the EMBED
  effort shipped, **`ascript::embed` is the semver-contracted host embedding API** —
  it carves a STABLE embedding surface out of this tier. Everything *else* in the
  crate (the VM, `Interp`, the compiler, the stdlib internals) remains INTERNAL.
- **Everything under `superpowers/` and `bench/`** — design records, plans, and
  benchmarks, not a public interface.

## Deprecation policy

**Pre-1.0**, no deprecation *period* is required: a breaking change may ship in one
minor release, but **only together with** the corpus migration and migration notes
(§ [Language version](#language-version)). Where a cheap compatible bridge exists —
aliasing an old name for one release — the implementation **SHOULD** prefer it; where
a clean bridge is not feasible (a NUM-style semantic break), it **MUST** break
*loudly* (a clear diagnostic), never silently.

**Post-1.0** (recorded now, **activated at 1.0**): removals follow
**deprecate-then-remove across a major version**, with a deprecation diagnostic
emitted in the interim release before removal.

## The road to 1.0

The criteria below are an **owner-editable**, living checklist — proposed, not
promised. 1.0 is declared when the owner judges the list complete.

- [ ] **Spec complete & green** — all 16 chapters published, each verified against
      the implementation, with `tests/spec_drift.rs` and the NAV bijection green in
      CI.
- [ ] **Stability soak** — **3 consecutive months** with no breaking change to the
      STABLE surface merged (the clock resets on any such merge).
- [ ] **Performance campaign closed** — `goal-perf.md` specs merged or explicitly
      parked; the Gate-12 floor (specialized/tree-walker geomean ≥ 2×) holds;
      headline numbers recorded under `bench/`.
- [ ] **EMBED verdict recorded** — the embedding API shipped-stable or explicitly
      deferred post-1.0. *(With `ascript::embed` shipped, this trends toward
      satisfied — see the INTERNAL tier note.)*
- [ ] **WASM spike verdict recorded** — GO / NO-GO per its Phase-0 gate.
- [ ] **Registry decision recorded** — a package registry ships, or the bare-version
      dependency source stays reserved at 1.0.
- [ ] **Fuzzing clean** — the `aso_roundtrip` nightly streak at the BIN bar (≥ 7
      consecutive ≥ 4 h crash-free runs) and zero differential-fuzzer divergences
      across the soak window.
- [ ] **EXPERIMENTAL list resolved** — every item in the EXPERIMENTAL tier promoted
      (spec'd into STABLE) or explicitly stamped post-1.0.
- [ ] **Zero recorded carry-forward bugs** — any deferred defect (e.g. the recorded
      `recover` anonymous-`fn`-expression defect) closed by 1.0.
- [ ] **Docs at parity** — the documentation drift suites green; README / landing
      repositioned for a 1.0 audience.
- [ ] **Process exercised** — at least one RFC-lite has run end-to-end (proposal →
      verdict → spec update).
- [ ] **Conformance suite frozen** — the 1.0 suite snapshot tagged; four-mode
      identity green on it in both feature configurations.

## Changing the language

Language evolution runs through a deliberately light **RFC-lite** process. The
existing campaign cadence — **spec → independent review → lock → plan → implement →
review → merge** — *is* the change-management process; RFC-lite formalizes only its
**front door** for language-surface changes.

A change is **RFC-bearing** iff it:

1. changes **STABLE** spec'd behaviour, **or**
2. adds **language surface** (grammar, AST, or a new value kind), **or**
3. **promotes or demotes** a stability tier.

Stdlib additions inside existing rules, **bug fixes toward spec'd behaviour**, and
INTERNAL changes are **not** RFC-bearing. (A fix that makes the implementation match
what a chapter already says never needs an RFC — it is a conformance repair.)

The process and the one-page template live under **`superpowers/rfcs/`**: open an RFC
as a numbered file via PR, the owner records a verdict, and on **Accept** the RFC
graduates into a full design spec and the normal cadence takes over. The implementing
PR updates the affected `docs/content/spec/` chapters; the RFC remains the permanent
decision record. See `superpowers/rfcs/README.md`.

## Conformance

This chapter is **policy**, not behaviour, so its "conformance" is the set of
**governance guardrails** that keep the policy honest and the spec in lockstep with
the implementation:

- `tests/spec_drift.rs` — the spec-drift tripwires. It asserts that all **16** spec
  chapters exist with a `## Conformance` section, that every example/test path a
  chapter cites resolves on disk, and that **every** named `grammar.js` rule appears
  in `docs/content/spec/grammar.md` (so a grammar change that skips the spec fails
  CI). Each check is backed by a deliberate-mutation self-test.
- `tests/docs_drift.rs` — the documentation-drift tripwires, including the NAV ⇄
  `docs/content` bijection that makes this chapter (and every spec page) reachable
  from the docs sidebar and search.
- `tests/vm_differential.rs` — the four-mode byte-identity battery that pins the
  STABLE *behaviour* this chapter governs (see the [conformance suite](conformance)).

Verified directly:

- `cargo test --test spec_drift` → all checks green (16/16 chapters, grammar covered,
  citations resolve, mutation self-test passing).
- `cargo test --test docs_drift` → green (NAV bijection over the 16 spec entries).
