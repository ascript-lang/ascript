# AScript Specification — Notation & Conformance

This is the **normative specification** of the AScript language. It describes the
language as **implemented** by the reference toolchain (`ascript`, crate version
0.6.0): every normative claim in these chapters is true of the binary you can
build from this repository, and each chapter's `## Conformance` section cites the
examples and tests that demonstrate it.

The chapters under `docs/content/spec/` are the authority. The tutorial pages
under [the language guide](../language/syntax) and the
[standard-library reference](../stdlib/collections) overlap in topic but never in
authority: where a guide page and this specification disagree, the specification
governs and the disagreement is a documentation bug.

This chapter is written first because every other chapter uses its terms.

## Status & versioning

The **language version is the crate version**: AScript is at **0.6.0** as of this
edition. Before 1.0, a breaking change to a STABLE part of the surface (see the
*Stability* chapter) requires a minor version bump with migration notes. Each
chapter is verified against the implementation at edition time; a chapter MUST NOT
assert behavior that the reference implementation does not exhibit.

The specification is NORMATIVE. The
historical design record is `superpowers/specs/2026-05-29-ascript-design.md`; it
documents intent, while this specification documents the realized language.

## Requirement words

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHALL NOT**,
**SHOULD**, **SHOULD NOT**, **MAY**, and **OPTIONAL** in this specification are to
be interpreted in the RFC 2119 sense:

- **MUST** / **REQUIRED** / **SHALL** — an absolute requirement. A conforming
  implementation that violates a MUST is non-conforming.
- **MUST NOT** / **SHALL NOT** — an absolute prohibition.
- **SHOULD** / **RECOMMENDED** — a requirement that MAY be overridden in
  particular circumstances, with the full implications understood.
- **MAY** / **OPTIONAL** — a genuinely optional behavior.

Normative statements are written in the present tense ("a panic unwinds to the
host"), one concept per section.

## Core terms

These terms are used throughout the specification.

- **Value** — a runtime datum. AScript has roughly sixteen user-facing value
  **kinds** (`nil`, `bool`, `int`, `float`, `decimal`, `string`, function, array,
  object, map, set, bytes, regex, native handle, enum variant, class/instance,
  plus `future`, `generator`, and structural `interface` descriptors). The
  *Values* chapter is the authoritative inventory.
- **Kind** — the runtime category of a value, as reported by the `type(x)`
  builtin. Kinds are disjoint; there is no implicit cross-kind coercion.
- **Isolate** — a single, independent runtime instance: its own heap, its own
  garbage collector, its own event loop. The main program runs in one isolate;
  each worker (the *Concurrency* chapter) runs in its own. An isolate is single-threaded and
  shares no memory with any other; values cross between isolates only by a
  structured deep copy through the serializer **airlock** (or, for frozen
  `shared` values, by reference).
- **Engine** — an implementation that executes AScript programs. The reference
  toolchain ships **two engines** behind **two front-ends**: a bytecode VM (the
  default and production engine — a CST front-end compiles to bytecode that the
  VM interprets) and a tree-walking interpreter (the legacy engine, retained as
  the differential oracle and reachable with `--tree-walker`). The two engines
  MUST exhibit identical observable behavior.
- **Four-mode / differential** — the reference toolchain runs the same program
  in four modes: the tree-walker, the specialized VM, the generic VM
  (`--no-specialize`), and from compiled `.aso` bytecode. These four modes MUST
  produce byte-identical observable behavior. The *Conformance* chapter
  formalizes this as the conformance criterion; the differential test
  battery checks it continuously.
- **Tier-1 error** — a *recoverable error value*. Fallible operations return a
  `[value, err]` pair (`error` is `object | nil`); the error is an ordinary
  value, not a panic. The `?` operator early-returns such a pair. See the
  *Errors* chapter.
- **Tier-2 panic** — an *unrecoverable bug* (a wrong type, a bad arity, an
  undefined name, a contract violation). A panic unwinds to the host, prints a
  source-pointed diagnostic, and exits non-zero; it is caught only by `recover`
  at a host boundary. See the *Errors* chapter.
- **Capability** — a coarse permission (`fs`, `net`, `process`, `ffi`, `env`)
  governing access to operating-system resources. Capabilities are opt-out:
  all are granted by default. See the *Capabilities* chapter.

## Behavior categories

AScript distinguishes three categories of behavior. Every normative section in
this specification falls into the first; the second and third are named
explicitly where they occur.

- **Implementation-defined** behavior is chosen by the implementation and
  documented (for example, the `std/intl` locale-data subset, or best-effort
  HTTP response trailers). A conforming implementation MUST document its choice.

- **Unspecified** behavior may be any of an identified set, with no documentation
  duty (for example, the interleaving of concurrently scheduled tasks, or the
  OS scheduling of worker isolates). A program MUST NOT depend on a particular
  resolution of unspecified behavior.

- AScript has **no undefined behavior**. Every erroneous condition is either a
  Tier-1 error value, a Tier-2 panic, or a clean compile/verification error.
  Silent wraparound, truncation, or coercion is a conformance bug, never
  latitude: integer arithmetic traps on overflow rather than wrapping (the
  wrapping operators `+% -% *%` are the explicit opt-in), and a wrong-kind
  operand is a panic rather than a coerced result.

## Conformance (overview)

An implementation of AScript **conforms** iff, over the adopted conformance suite,
it produces byte-identical observable behavior — standard output, exit status,
and panic/diagnostic messages — to the suite's recorded goldens and the reference
implementation. The *Conformance* chapter gives the formal
definition; the criterion is the **four-mode byte-identity** described above.

One **engine asymmetry** is documented and intentional: bytecode-capacity limits
(a constant pool, prototype table, or import table exceeding `u16::MAX`, or a jump
displacement exceeding 32 KB) are VM-only clean compile errors. The tree-walker
has no bytecode and therefore no such limits. This asymmetry concerns only the
*size* of admissible programs, never the *behavior* of admissible programs, and is
pinned by the VM-limits battery.

## Aspirations

A formal operational semantics for AScript is recorded as a future possibility,
not a v1 deliverable. Today the differential oracle — the tree-walking
interpreter, kept byte-for-byte identical to the VM — IS the executable semantics:
where this prose and the oracle disagree, the oracle is presumed correct and the
prose is a defect (see the *Conformance* chapter).

## Conformance

The terms and criteria in this chapter are exercised by the differential battery
and the documented engine asymmetry:

- `tests/vm_differential.rs` — the four-mode differential battery: the corpus,
  the recorded goldens, and targeted equality/short-circuit/recursion suites run
  in the tree-walker, specialized VM, generic VM, and `.aso` modes and are
  asserted byte-identical. This is the executable form of the conformance
  criterion defined above.
- `tests/vm_limits.rs` — pins the documented VM-only engine asymmetry
  (bytecode-capacity errors are clean compile errors on the VM; the tree-walker
  has no such limits).
- `examples/hello.as` and `examples/all_features.as` — runnable programs in the
  adopted corpus that the differential battery executes in all four modes.
