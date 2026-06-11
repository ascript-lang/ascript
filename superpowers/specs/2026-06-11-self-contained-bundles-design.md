# AScript Self-Contained Bundles — Module Archive, Tree-Shaking & Capability Embedding — Design (BNDL)

- **Status:** Proposed (no branch yet — design only)
- **Date:** 2026-06-11
- **Code:** BNDL (follow-on to BIN — native single-binary distribution)
- **Depends on:** the shipped BIN feature (`src/bundle.rs` footer codec, `build_native`,
  `try_run_embedded`/`run_embedded_aso` in `src/lib.rs`/`src/main.rs`); the VM import path
  (`Op::Import` + `load_file_module`, `src/vm/run.rs`); the `.aso` serializer
  (`src/vm/aso.rs`) + verifier (`src/vm/verify.rs`); the capability model (`src/stdlib/caps.rs`,
  `compose_caps` at `src/main.rs:286`); the resolver (`src/syntax/resolve/`) for reachability.
- **Engines:** both (tree-walker oracle == specialized-VM == generic-VM, byte-identical).
  Tree-shaking and archiving change *what bytes are produced/loaded*, never *observable
  behavior* — guarded by the four-mode differential plus a shaken-vs-unshaken tripwire.
- **Breaking:** no. A zero-import program still emits a bare single-chunk `.aso` (today's
  format). The archive container is additive and version-tagged. `ASO_FORMAT_VERSION` and a
  new `ARCHIVE_VERSION` bump.

---

## 1. Motivation

Two defects surfaced during the post-BIN review:

1. **Bundles are not self-contained for multi-file programs.** `build_native` compiles only
   the *entry* file (`compile_verified_aso_bytes(file, true)`); a relative `import "./x"`
   resolves at runtime via `load_file_module`, which reads the sibling `.as`/`.aso` **from
   disk relative to the executable** (`src/vm/run.rs:705`). A native binary with any `./`
   import therefore silently depends on external files shipped alongside it, and a package
   import expects the `$ASCRIPT_CACHE` store to exist on the target machine. The "single
   self-contained binary" promise holds only for single-file scripts. The same is true of a
   plain `ascript build` `.aso`: it references its imports externally, it does not embed them.

2. **Bundled programs run with all capabilities granted (N4).** `run_embedded_aso` passes
   `caps: None` and never consults the build-time `--deny`/`ascript.toml [capabilities]`. A
   program developed and tested under `--deny net` ships fully unsandboxed, with no build-time
   warning. Capability state must live *inside* the signed image, not in an external,
   deletable sidecar.

A third, owner-requested goal rides along: **tree-shaking.** Embedding the whole import graph
naively would bloat artifacts with code that is never used. We embed only the *reachable*
top-level declarations.

Non-goal restated for clarity: `std/*` modules are **native Rust**, already linked into the
runtime via the registry. Tree-shaking neither embeds nor removes std — this feature concerns
only the **user + package `.as`/`.aso` module graph**.

## 2. Overview of the solution

One new artifact, the **module archive**, becomes what both `ascript build` (portable) and
`ascript build --native` (embedded) produce when a program has file/package imports. It carries
a manifest (entry id, embedded `CapSet`, shake report digest) and a table of per-module,
individually-verified, tree-shaken chunks. At runtime an in-memory module map is consulted
before disk, so a bundled or archived program never needs its source tree.

Three layered capabilities, built in order:

- **Archive format + in-memory loader** (§3, §6) — self-containment.
- **Tree-shaker + build report** (§4) — minimal embedded code.
- **Capability embedding** (§5) — closes N4.

## 3. The module archive container

### 3.1 When it is produced

- A program with **no file/package imports** → today's bare single-chunk `.aso` (no
  regression, no archive).
- A program **with** file/package imports → an `ASCRIPT_ARCHIVE`, produced by:
  - `ascript build` → a portable `.aso` archive (carries its whole module graph),
  - `ascript build --native` → the archive becomes the footer payload (`bundle.rs` unchanged
    except `validate_footer` now points at an archive rather than a lone chunk).

### 3.2 Layout

```
magic("ASCRIPTA") · archive_version:u16
manifest:
  entry_module_id:u32
  caps: CapSet            (bits:u8 + fs_scope:Option<…> + net_scope:Option<…>, length-prefixed)
  shake_report_digest     (informational; the human report is emitted to stderr at build)
module_table:
  count:u32
  [ logical_path_key:string · verified_chunk_bytes:len-prefixed ] × count
```

Each `verified_chunk_bytes` is a normal `.aso` chunk: the existing
`Chunk::from_bytes_verified` trust boundary applies **per module** on load, unchanged. The
archive reader bounds-checks every length against remaining input (mirroring `aso.rs`'s
allocation-bomb clamps) and never trusts the count or any key/blob length blindly.

### 3.3 Module identity & keys

The `logical_path_key` is the **canonical logical specifier** a module is reached by — the same
key `load_file_module` already computes (`as_path.canonicalize()` → `aso` → requested stem,
`src/vm/run.rs`). For a package module it is the store-relative logical id, not the absolute
`$ASCRIPT_CACHE` path, so a bundle built on one machine resolves on another. Identity is
preserved exactly so circular imports and the once-only side-effect cache behave as today.

### 3.4 Versioning

A new `ARCHIVE_VERSION` (independent of `ASO_FORMAT_VERSION`) tags the container. The embedded
chunks carry their own `ASO_FORMAT_VERSION`. A version or magic mismatch is a clean
`AsoError`, never a panic.

## 4. Tree-shaking

### 4.1 Where

At the **resolved-graph level, before per-module bytecode compilation.** The resolver already
records binding references; we run reachability over that, then the compiler emits only the
reachable top-level declarations for each module. We do **not** do bytecode-level DCE (it would
require rewriting jump targets, const pools, and proto indices — high risk against the
byte-identical invariant).

### 4.2 Granularity

The **top-level binding** is the unit. A top-level `fn` / `class` / `enum` / `interface` /
`const` is droppable because it is inert until referenced. A **reachable class is kept whole**
(its methods dispatch dynamically; sub-method shaking is unsound) along with its superclass
chain, implemented interfaces, and referenced enum variants.

### 4.3 The reachability worklist

- **Roots** = the entry module's top-level statements. *Every* top-level statement runs on
  import, so all are roots and everything they reference is reachable.
- **Side-effectful top-level statements are always kept** — a bare call, a `let x =
  sideEffect()`, top-level control flow — because dropping them would change what runs on
  import. Their references are roots. Only *inert, unreferenced declarations* are dropped.
- **Transitive closure:** a kept function/class body's references mark their targets reachable,
  crossing module edges via imports.
- **Named imports** (`import { sqrt } from "./m"`) → mark exactly `sqrt` reachable in `m`,
  recurse into its definition.
- **Namespace imports** (`import * as m`): if *every* use is a static `m.<literal>`, treat each
  as a named access and shake the remainder of `m`. If **any** `m[expr]` dynamic index appears,
  **or `m` itself escapes** (returned, stored in a structure, passed as an argument), **pin all
  of `m`'s exports** — the Approach-C whole-module fallback, scoped to that one module.

### 4.4 Soundness — zero false drops

Anything the analysis cannot prove reachable-or-not resolves to **keep**: re-exports, a binding
value escaping, dynamic namespace access, any reflective-shaped pattern. The shaker may
over-include; it must never under-include. The differential tripwire (§7) is the backstop.

### 4.5 Determinism

The shake result is a deterministic function of the source (stable module and binding
ordering). Builds are reproducible; the archive is byte-stable.

### 4.6 Build report

Per module, the build emits (to stderr) what was **dropped** and what was **kept-because-
unprovable**, each with a reason and source span — e.g.
`kept all of ./util.as: namespace-indexed dynamically at app.as:42`. This is the user's lever
to refactor toward named imports and shrink the artifact further. A digest of the report is
stored in the manifest (§3.2) for `ascript inspect`-style tooling; the full human report is not
embedded.

## 5. Capability embedding (closes N4)

### 5.1 Build time

`build_native` and `ascript build` compute the `CapSet` from the **same** source a normal run
uses — `compose_caps` over the CLI (`--deny`/`--sandbox`/`--deny-net`/`--deny-fs`) plus the
nearest `ascript.toml [capabilities]` table (`src/main.rs:286`). The **full** `CapSet` — the
`bits: u8` and the variable-length `fs_scope`/`net_scope` carve-outs — serializes into the
archive **manifest** (§3.2). The manifest is the variable-length home the 32-byte footer could
not provide.

### 5.2 Runtime

`run_embedded_aso` and the archive `.aso` loader read the manifest `CapSet` and call
`interp.set_caps(...)`, replacing the hardcoded `caps: None` (`src/lib.rs:1106`) that is the N4
escalation bug.

### 5.3 Launch-time override — `ASCRIPT_DENY`, monotone-subtract only

Embedded caps are a **fixed ceiling**. A bundled binary forwards all argv to the program
(unchanged), so caps are **not** overridable via argv flags. An optional **`ASCRIPT_DENY`
environment variable** may **only subtract further** (monotone; never re-grants), consistent
with the irreversible capability model. This gives ops a deny-more escape hatch without a
rebuild while keeping argv cleanly owned by the program. Parsing reuses the same comma-separated
cap-name grammar as `--deny`.

## 6. Runtime: the in-memory module loader

An `Interp`-held `module_archive: Option<ModuleArchive>` maps `logical_path_key → verified
Chunk`. `load_file_module` (`src/vm/run.rs:705`) consults it **before** the disk
`stat`/`read`:

- **Hit** → use the embedded chunk, skip disk entirely.
- **Miss** → fall through to today's exact disk path (so mixed dev/partial scenarios and
  on-disk `.aso`/`.as` still work).

std stays native (resolved via the registry, never the archive). Circular imports and the
once-only side-effect cache are unchanged because module identity (§3.3) is preserved.

**Worker parity:** the entry already stashes `worker_aso_bytes` for code-shipping; this extends
to ship the whole archive so a `worker fn`/`worker class`/`worker fn*` inside a bundled app
resolves embedded modules instead of failing on disk. Covered by extending the existing
`native_worker_bundle_parity` test (`tests/native.rs`).

## 7. Testing — the gating invariants

- **Four-mode byte-identical over multi-module programs:** tree-walker == specialized-VM ==
  generic-VM == archive run, in both feature configs.
- **Shaken-vs-unshaken differential (load-bearing):** a tree-shaken archive must produce
  byte-identical output to the same program run unshaken from disk. If shaking ever changes
  behavior, this trips — and it means the reachability analysis dropped something live.
- **Archive deserialization fuzzing:** untrusted bytes, per-module bounds, manifest parsing,
  allocation bombs — added to the cargo-fuzz harness alongside `aso_roundtrip`.
- **Capability enforcement:** a bundle built with `--deny net` actually denies at runtime; an
  `ASCRIPT_DENY=fs` launch subtracts further; a granted cap cannot be re-granted.
- **Version bumps:** new `ARCHIVE_VERSION`; `ASO_FORMAT_VERSION` bumped if any chunk-layout
  change lands alongside.

## 8. Implementation plan structure

One master plan, phased (folds the standing bug-fix work in as Phase 0 per owner direction):

- **Phase 0 — Bug fixes** (independent of the feature; can ship first):
  - Correctness: i64/float boundary (`value.rs` ×3), negative-integer enum backing
    (`compile/mod.rs`), `.aso` range-`step` drop (`aso.rs`), or-pattern resolver gap
    (`syntax/resolve/mod.rs`), legacy formatter parameter defaults (`fmt.rs`), `synth_array`
    double-synthesis (`check/infer/pass.rs`), LSP `did_rename` stale index (`lsp/server.rs`),
    CST `return;` spurious error node (`syntax/parser.rs`).
  - Robustness/verifier: `SetGlobal` verifier stack-depth (`vm/verify.rs`),
    `VariantElem`/`MatchVariantArity` operand bounds (`vm/verify.rs`).
  - Security: HTTP response header CRLF injection (`stdlib/http_server.rs`), git arg injection
    missing `--` (`pkg/fetch.rs`), `string.repeat`/`reader.read` non-finite count guards.
  - Durability/determinism: workflow log atomic write (`stdlib/workflow.rs`),
    `clock_monotonic_ms` replay-mismatch handling (`det.rs`), `crypto.hashPassword` seeded-RNG
    under replay (`stdlib/crypto.rs`).
  - DAP: unbounded `Content-Length` cap (`dap/proto.rs`), `scopes` `frame_id` overflow
    (`dap/server.rs`), double-`launch` state corruption (`dap/server.rs`).
  - BIN: post-confirmation payload-read error reporting (`main.rs`), `build_native`
    double-bundle stub stripping (`lib.rs`), output-path TOCTOU temp-then-rename (`lib.rs`).
  - (N4 is **not** here — it is closed by Phase 3.)
- **Phase 1 — Archive format + in-memory loader** (§3, §6).
- **Phase 2 — Tree-shaker + build report** (§4).
- **Phase 3 — Capability embedding** (§5).
- **Phase 4 — `--native` + portable `ascript build` archive wiring, worker parity, full test
  matrix** (§7).

## 9. Execution standards — the binding Definition of Done

These apply to **every task** in the plan (Phase 0 bug-fixes included). A task is not "done"
until all of them hold. **Nothing is deferred — no `TODO`, no "follow-up", no silent drop, no
"out of scope for now."** If a deferral seems unavoidable, it is escalated to the owner as a
decision, never taken unilaterally.

### 9.1 Per-change deliverables (all required, per change)

- **Unit tests** — inline `#[test]`/`#[tokio::test]` covering the change, its edge cases, and
  its failure modes. A bug-fix gets a regression test that fails before and passes after.
- **`.as` example file(s)** — a runnable `examples/*.as` (and an `examples/advanced/*.as`
  production-shaped, fully error-handled variant where the feature warrants it), exercised by
  the conformance corpus and verified with `target/release/ascript run <file>`.
- **Docs** — the matching `docs/content/**` page updated; a **new** page is added to the `NAV`
  array in `docs/assets/app.js` (sidebar + cmd-K both derive from it, or the page is
  unreachable). README/CLI tables updated where surface changes.
- **Blast-radius assessment** — every change is preceded by an explicit blast-radius pass: who
  calls this, what serializes it, which engine paths touch it, what `.aso`/grammar/LSP surface
  it moves. Everything the assessment surfaces is fixed within the same task — not noted for
  later.

### 9.2 Cross-cutting correctness (the project's standing checklist, enforced)

- **Two parsers + tree-sitter.** Any surface-syntax change updates the legacy `parser.rs`, the
  CST parser, AND `tree-sitter-ascript/`, with `parser.c` regenerated (`tree-sitter generate
  --abi 14`). `tests/treesitter_conformance.rs` + `tests/frontend_conformance.rs` stay green.
- **Exhaustive matches.** New `ExprKind`/`Pattern`/`Stmt` variants get arms in `interp.rs`,
  `fmt.rs`, the CST formatter (`syntax/format/`), and `ast.rs` `Display`.
- **Formatter** round-trips and is idempotent for any new/changed surface; no comment loss.
- **LSP** works for any new surface: hover/inlay types, go-to-def, find-references, rename,
  and workspace-symbol stay correct (and the workspace index stays fresh).
- **Both engines byte-identical** across the whole corpus + goldens
  (`tests/vm_differential.rs`), both feature configs. Fix the engine, never relax the
  assertion.
- **`.aso`/`ARCHIVE` versioning** bumped on any layout change, with `verify.rs` updated.
- **Grammar publish** (`./scripts/sync-grammar.sh` + editor-pin bumps) when
  `tree-sitter-ascript/**` changes.

### 9.3 Production-grade bar

- `cargo clippy --all-targets` **and** `--no-default-features --all-targets` both clean.
- No Rust `panic!`/`unwrap`/`expect`/unchecked index reachable from script or untrusted input
  (`.aso`/archive bytes, DAP messages, network, FFI) — those are clean typed errors or
  recoverable Tier-2 panics.
- No `RefCell` borrow held across `.await` or across re-entrant user-code calls.
- Full error handling; no half-states; deterministic, reproducible output.

### 9.4 Discovered bugs

**Any bug found along the way — related or not — is fixed in this body of work**, with its own
regression test and (if it touches a user-visible surface) example + docs. Discovery is logged
in the plan so the fix is tracked, never lost.

### 9.5 Subagent-driven development workflow

Each task is executed by a **fresh implementer subagent**, then independently verified by **two
distinct reviewer subagents before the task is accepted**:

- a **code-quality reviewer** — idiom, structure, naming, maintainability, clippy/test hygiene,
  the §9.3 production bar; it runs the commands and probes edges, it does not just read.
- a **spec-&-plan-adherence reviewer** — checks the change against THIS spec and the plan task:
  scope met in full, nothing deferred, all §9.1 deliverables present, acceptance criteria
  satisfied.

A task is accepted only when **both** reviewers pass. Disagreement or a found defect bounces the
task back to a fresh implementer with the review notes. Per-task acceptance criteria are written
into the plan so adherence is checkable, not subjective.

**Per-phase holistic review.** When all tasks in a phase are accepted, a dedicated reviewer
subagent reviews the phase's changes **as a whole** — cross-task integration, consistency,
emergent blast radius, and that the phase's combined surface meets the spec — before the next
phase starts. A holistic finding bounces back into the phase as a tracked task; the phase is not
closed until the holistic review passes.

**Progress tracking.** The plan is a living checklist: every task and every per-task deliverable
(§9.1) is a checkbox, ticked only when delivered and accepted. Phase-completion and the
holistic-review pass are themselves checkboxes. Unchecked boxes are the single source of truth
for what remains — the effort is done only when every box is ticked.

### 9.6 Definition of Done (whole effort)

Every phase complete, every task accepted by both reviewers, full suite green in both feature
configs, clippy clean in both, docs/examples/LSP/tree-sitter/formatter all updated and
verified, zero open `TODO`/deferral, zero known unfixed bugs. **Nothing left to do.**

## 10. Open risks

- **Reachability precision vs. AScript's dynamic features.** The conservative fallback (§4.4)
  bounds correctness risk to "over-include," but the shaken-vs-unshaken differential must run on
  a corpus rich in namespace imports, dynamic member access, re-exports, and escaping values to
  exercise the fallback paths.
- **Package logical-key stability.** §3.3's store-relative logical id must round-trip across
  machines; a hidden absolute-path leak would make bundles non-portable. Tested by building on
  one path layout and running from another.
- **macOS overlay signing.** Embedded caps in the archive are within the footer payload, which
  the ad-hoc signature does **not** cover (`build_native` signs `[0, codeLimit)` = stub only).
  Caps are therefore tamper-*evident* only if overlay signing is later extended. Acceptable for
  v1 (lowering one's own caps is not an attacker goal); documented, not silently assumed.
