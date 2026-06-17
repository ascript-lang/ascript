# AScript Warm Starts & Durable-Log Throughput — Design (WARM)

- **Status:** Implemented — merged to `main` (merge SHA in `goal-perf.md` EXECUTION LOG). All three
  units shipped; `ASO_FORMAT_VERSION`/`ARCHIVE_VERSION` unchanged; tree-walker untouched.
  **Deltas from spec recorded at merge:** (1) `collect_module_graph` is a *parallel re-derivation*
  of `compile_path_module_set` (not a literal extraction), kept equivalent by the §2.5 walk-drift
  tripwire — a prose correction, not a semantics change; (2) the §3 "ASO_FORMAT_VERSION still 27"
  references were stale at authoring time (the constant was already 29 via ELIDE) — the binding
  invariant is "WARM introduces no constant change vs `main`", which holds.
- **Date:** 2026-06-12
- **Code:** WARM (PERF campaign, `goal-perf.md` "Deployment & I/O throughput")
- **Depends on:** BNDL merged (the `ASCRIPTA` module-archive container + `compile_archive`
  reachability walk, `src/vm/archive.rs` + `src/lib.rs:1074`); the shipped SP6 package cache
  (`src/pkg/{cache,hash}.rs` — `$ASCRIPT_CACHE`, content-addressed store, `asum1`); shipped DBG
  (the optional strippable `.aso` debug-section precedent, `src/vm/aso.rs` v26, and the
  `build --strip` CLI shape); the SP9 workflow engine (`src/stdlib/workflow.rs`, `src/det.rs`)
  including the a56fbf2 atomic temp+rename log write.
- **Depended on by:** nothing. (The Unit-B PGO section is the designated *carrier* that a future
  DECODE/JIT feedback consumer would extend — recorded affinity, not a dependency.)
- **Engines:** Units A and C are CLI/stdlib work — N/A to the engine split (stdlib behavior is
  shared by both engines by construction). Unit B touches VM warm-state **seeding only**: the
  seeded caches are the *same* `chunk.{field_ics,arith_caches,global_caches}` / `Vm.shapes`
  entries runtime warmup would build, behind the *same* runtime guards — PGO-seeded vs unseeded
  must be **byte-identical** over the corpus (a new differential mode + fuzz axis, Gate 15).
  The tree-walker is untouched by all three units.
- **Breaking:** no. **`ASO_FORMAT_VERSION` stays 27 and `ARCHIVE_VERSION` stays 1** — §3.4
  derives why the optional PGO section needs no bump (the archive codec's verified
  trailing-byte tolerance becomes a *pinned, contractual* trailing-sections rule). The
  workflow `durability` option keeps its existing values and default; `"group"` is additive.
  One deliberate hardening (§4.3): an *unknown* `durability` string becomes a Tier-2 error
  instead of silently meaning `"fsync"` — garbage-input-only, both engines symmetric.

---

## 0. Read this first — what the code actually does (three grounded corrections)

This spec was drafted against the live code, and three load-bearing facts differ from the
campaign-level summary in `goal-perf.md`/the unit briefs. They reshape the design, so they come
first:

1. **The workflow log is NOT per-event-appended today.** The *only* `write_log` call site is
   `finish_workflow` (`src/stdlib/workflow.rs:524`): the **whole** event log is serialized from
   memory and written **once per `run`/`resume`**, as an atomic temp+rename snapshot
   (`write_log`, `workflow.rs:759` — the a56fbf2 fix) with `sync_all()` (macOS `F_FULLFSYNC`)
   unless `durability: "buffered"`. The 96%-fsync `workflow_loop` profile
   (`bench/PROFILING_RESULTS.md`) is **3 000 separate `run()` calls each paying one
   file-F_FULLFSYNC + dir-fsync + unlink** — a per-*commit* cost, not a per-event one.
   Consequence: today's default has a **mid-run durability hole** — a `kill -9` (or power loss)
   mid-workflow loses *every* event recorded so far, and `resume` re-executes the entire
   workflow from the top. Unit C's group mode is therefore not a relaxation of a per-event
   appender that doesn't exist; it **introduces** incremental appends (strictly *better* mid-run
   durability than the default) while making fsync a bounded, coalesced, documented policy.
2. **A `durability` option already exists** (`read_options`, `workflow.rs:367-389`):
   `"fsync"` (default) | `"buffered"`. Unit C extends this surface (adds `"group"`) rather than
   inventing a parallel `full|group|async` vocabulary. The brief's `"async"` (background-fsync)
   tier maps onto the existing `"buffered"` semantics (OS-asynchronous writeback, no explicit
   fsync) and is **not** added as a fourth mode (§7).
3. **The archive codec already ignores trailing bytes.** `ModuleArchive::decode`
   (`src/vm/archive.rs`) reads magic·version·manifest·module-table and returns without checking
   `pos == len` — undocumented but real tolerance. Unit B makes this **contractual** (a pinned
   test) and uses it: the PGO section is a self-described *trailing* section, so a current
   (pre-WARM) runtime runs a PGO-carrying archive correctly by ignoring it (= warm normally),
   and **no `ARCHIVE_VERSION` bump is needed** (§3.4).

## 1. Motivation & evidence

Three independent latency/throughput taxes, none of them engine-dispatch problems:

- **Cold starts.** `ascript run file.as` re-parses, re-resolves, and re-compiles the entry
  *every* run (`run_file_on_vm_with_packages`, `src/lib.rs:2024` → `compile_source` at
  `lib.rs:2040`), and every imported module again at `Op::Import` time. For a large project the
  whole front-end cost is paid on every invocation even when nothing changed. Unit A caches the
  verified compiled artifact, keyed airtight (§2).
- **Warm-up.** A freshly-loaded program starts with cold inline caches, cold adaptive-arith
  sites (`WARMUP_THRESHOLD = 8`, `src/vm/adapt.rs:44`), cold global caches, and an empty
  `ShapeRegistry` — re-derived identically on every run from the same training-shaped traffic.
  Unit B records the warmed state once at build time and pre-seeds it at load (§3), with a
  soundness story that makes a wrong seed a *miss*, never wrong behavior.
- **Durable-log fsync.** `workflow_loop` is 96% fsync (`F_FULLFSYNC` 82%,
  `bench/PROFILING_RESULTS.md` — "workflows are an fsync problem, not a language problem").
  Unit C makes durability an explicit policy with a precise loss-window contract (§4); the
  default never silently relaxes (pillar).

All three are opt-in or invisible-by-default, kill-switchable, and measured with same-session
A/B (Gates 16/18). No speedup is promised; expectations are stated, results measured (§6).

---

## 2. Unit A — content-addressed compile cache for `ascript run`

### 2.1 Mechanism overview

On `ascript run file.as` (the default VM path), the CLI consults a **compile cache** under the
existing SP6 cache root before compiling:

- **Hit** → load the cached, *verified* artifact and run it through the existing
  `run_verified_aso` magic-routing path (`src/lib.rs:1744` — `ASCRIPTA` → archive runner,
  `ASO\0` → single chunk). Zero parse/resolve/compile work.
- **Miss** → compile exactly as today, then publish the artifact + a manifest atomically into
  the cache, and run **the freshly-compiled artifact through the same hit path** (so hit and
  miss runs exercise one code path — there is no "first run behaves differently" mode skew).
- **Any cache-layer failure** (unreadable store, hostile/corrupt entry, keying error, exotic
  import graph the walk rejects) → **fail open to today's uncached compile-and-run**. The cache
  is an optimization layered over an unchanged semantic path; it is never the *reason* a run
  errors.

### 2.2 The cache key — an explicit struct, serialized + hashed

A stale hit is a **wrong-code bug of Gate-1 severity**; the key is therefore an explicit,
versioned struct so every input to codegen is enumerated and additions are mechanical:

```rust
/// Everything that can change WHAT BYTES `ascript run <entry>` would compile.
/// Serialized canonically (field-tagged, sorted flags) and sha256'd to form the
/// cache LOCATION key. The schema itself is versioned by KEY_SCHEMA ("ck1"),
/// mirroring the pkg store's `asum1-` algorithm-prefix convention
/// (src/pkg/hash.rs:23) — rotate to "ck2" on any schema change.
struct CompileCacheKey {
    key_schema: &'static str,        // "ck1"
    aso_format_version: u32,         // src/vm/aso.rs:167 (27)
    archive_version: u16,            // src/vm/archive.rs:27 (1)
    binary_stamp: BinaryStamp,       // §2.3 — invalidates on a rebuilt compiler
    flags: Vec<(String, String)>,    // codegen-relevant flags, sorted; v1:
                                     //   [("debug","true"), ("shake","false")]
                                     //   (future: ("elide", …) when ELIDE lands)
    entry_path: String,              // CANONICAL entry path — deliberate, §2.4
    package_map_digest: [u8; 32],    // sha256 over the canonically-serialized
                                     // resolved PackageMap (specifier → store dir),
                                     // so a lockfile/resolution change = miss
}
```

**Source identity is validated, not key-embedded** (the two-level scheme, §2.5): the location
key above selects a cache *slot*; the slot's **manifest** records the exact reachable source
file set with per-file sha256 digests, and a hit requires every listed file to re-hash equal.

### 2.3 The binary stamp — a rebuilt compiler invalidates

`binary_stamp = (CARGO_PKG_VERSION, current_exe().len, current_exe().mtime)`.

Rationale: invalidation must err toward **miss**. Hashing the whole multi-MB binary per run
would eat the win; version-string alone would stale-hit across same-version dev rebuilds (the
common case while hacking on the compiler — exactly when codegen changes). Any rebuild rewrites
the executable → new mtime/len → miss (correct direction); a *copied* binary gets a fresh mtime
→ spurious miss (safe). A false *hit* requires an adversarially mtime-preserved, same-length,
different-content binary — not an accidental failure mode, and the cache is local-machine-only
(§7). Precedent: the pkg store versions its *algorithm* via the `asum1-` prefix and treats
store entries as immutable; the compile cache versions its *key schema* via `ck1` the same way
and adds the binary stamp because — unlike package sources — the compiler itself is an input
to the artifact.

### 2.4 Canonical-key semantics: the entry path is part of the key (decided)

Same content at a **different path = MISS**, deliberately deviating from pure content
addressing. Grounds: the cached artifact embeds the DBG debug section (module **path** + text,
`src/vm/aso.rs` v26) and panic diagnostics render that embedded path. A content-keyed
cross-path hit would print *another file's* absolute path in its caret diagnostics — an
observable divergence from the uncached run, violating Gate-1 byte-identity. Path-in-key keeps
the artifact's embedded provenance always equal to the invoking path. (The artifact bytes are
still content-validated on every hit via the manifest re-hash; the path is *location*, the
content is *validity*.)

### 2.5 The two-level lookup (the ccache-direct-mode shape)

A one-level "hash the entry + all reachable sources" key has a chicken-and-egg cost: the
reachable set is only known by *parsing* every module to extract imports — paying a parse-walk
on every hit defeats the cache. The standard answer (ccache/sccache "direct mode") is
two-level:

1. **Location:** `compiled/<sha256(CompileCacheKey)>/` under the SP6 cache root
   (`cache_root()`, `src/pkg/cache.rs:26`) — a sibling namespace to `store/`, `git/`, `tmp/`.
2. **Validation:** the slot holds `manifest.json` — the reachable module list recorded at
   compile time: `[{ logical_key, path, sha256 }]` plus the artifact's own sha256 — and
   `program.aso` (the artifact). A **hit** = every listed file exists and re-hashes equal AND
   the artifact re-hashes equal. Any mismatch, missing file, or unparsable manifest = miss.

**Soundness of the manifest scheme:** the reachable file set is a deterministic function of
the file *contents* (import specifiers are static strings; there is no search-path shadowing —
a relative specifier resolves to exactly one path, a package specifier through the PackageMap
which is key-embedded). If every recorded file is byte-identical, its import statements are
identical, so the reachable set is identical and exactly the recorded one. A file *not* in the
set cannot affect compilation; a new import can only appear by editing a recorded file, which
changes that file's digest → miss → recompile records the new set. Deleted recorded file →
re-hash fails → miss.

The file set is produced by the **same reachability walk `compile_archive` performs**
(`src/lib.rs:1074`) — the plan extracts the walk into a shared helper so the keyed set and the
compiled set cannot drift. Package modules are hashed by file content uniformly (no `asum1`
shortcut in v1): immutable store packages re-hash stably for free, and **mutable `{path = …}`
dependencies are correctly covered** — an asum1-of-store shortcut would miss path-dep edits.

### 2.6 The cached artifact

The **unshaken, debug-carrying archive with a neutral capability floor**:

- `compile_archive_with_shake(entry, with_debug = true, shake = false)` — *unshaken* because
  `run` is the dev loop where identity with the from-source run must be absolute (shaking buys
  artifact size, irrelevant for a local cache, and removes a whole class of risk-adjacent
  coupling to the shaker); *with debug* so panic diagnostics keep line/path info (§2.4).
  Single-module programs cache the same 1-module archive (one artifact shape, one hit path).
- `archive.caps = CapSet::all_granted()` — the **neutral floor**. `run_verified_archive`
  composes the embedded floor with run-time caps by monotone intersection (`lib.rs:1841`);
  `all_granted ∩ runtime = runtime`, so a cached run's effective caps are *exactly* the
  CLI/manifest-composed set, byte-identical to the uncached run. Caps are deliberately **not**
  in the key (they do not affect codegen; they compose at run time). A cached artifact is
  **not** a distribution artifact (`ascript build` semantics differ: shaken, real caps floor)
  — it never leaves the cache directory.

### 2.7 Store hygiene, verification, eviction

- **Atomic publish:** stage in the cache `tmp/` (the pkg staging dir, `cache.rs:100`), write
  `program.aso` then `manifest.json`, fsync-free `rename` into `compiled/<key>/` —
  last-writer-wins; concurrent racers compile identical bytes (BNDL §4.5 builds are
  deterministic), so a race is benign. The a56fbf2 temp+rename discipline, applied to both
  files (manifest renamed **last**, so a half-published slot has no manifest → clean miss).
- **Verification on every hit:** the hit path runs the artifact through the **existing**
  `from_bytes_verified` trust boundary per module (the same verifier any `.aso`/archive run
  uses — the FUZZ-hardened reader). A verifier rejection = cache poisoning → **fail closed to
  recompile**: delete the slot, compile from source, re-publish. Never trust, never crash.
- **Eviction v1 (decided, minimal-honest):** **no auto-eviction.** New CLI:
  `ascript cache clean` (removes `compiled/` — the compile cache only; the pkg store is
  managed by `install`/`verify`) and `ascript cache dir` (prints the root, scriptability).
  Auto-GC (LRU by atime + a max-size config) is a **recorded follow-up** in `roadmap.md`,
  not silently dropped. Documented in `docs/content/cli.md`.

### 2.8 Scope & opt-out (permanent)

- **Scope:** the plain `ascript run <file>.as` VM path. Explicitly **uncached** (documented,
  not silent): `--tree-walker` (oracle path — never touched), `--inspect` (DAP wants the
  freshest source mapping), `--profile` (measurement runs must not measure the cache),
  `ascript test` (tree-walker), the REPL, `run <file>.aso` (already compiled).
- **Opt-out:** `--no-cache` flag + `ASCRIPT_NO_COMPILE_CACHE=1` — permanent kill switches
  mirroring the `--no-specialize`/`ASCRIPT_NO_SPECIALIZE` posture, not bring-up scaffolding.

### 2.9 Failure modes (enumerated)

| Failure | Behavior |
|---|---|
| Stale source anywhere in the graph | manifest re-hash mismatch → miss → recompile (the adversarial battery proves every edit class misses, §5) |
| Corrupt/poisoned artifact | `from_bytes_verified` rejects → slot deleted → recompile + republish (fail closed) |
| Corrupt/missing/garbage manifest | unparsable → miss (slot overwritten on republish) |
| Concurrent runs, same key | atomic rename, last-writer-wins, identical bytes — both run correctly |
| Cache root unwritable / IO error | fail open: run uncached, succeed anyway |
| Compiler rebuilt (any rebuild) | binary stamp mtime/len changes → miss |
| `ASO_FORMAT_VERSION` / `ARCHIVE_VERSION` / key-schema bump | key changes → miss (old slots become unreachable garbage for `cache clean`) |
| Lockfile / package re-resolution / path-dep edit | `package_map_digest` or file re-hash changes → miss |
| Same content, new path | miss by design (§2.4) |
| `touch` without content change | **hit** (digests, not mtimes, validate sources — tested) |

---

## 3. Unit B — PGO warm-state section in the archive manifest

### 3.1 What is recorded (and what is provably seedable)

The VM's warm state lives in per-chunk offset-keyed side tables
(`src/vm/chunk.rs:421-435`: `field_ics`, `method_ics`, `arith_caches`, `global_caches` —
`RefCell<OffsetMap<…>>`, never serialized today; `aso.rs:780-783` debug-asserts them empty on
write) plus the per-`Vm` `ShapeRegistry` (`src/vm/run.rs:87`) and `class_base_shapes`
(`run.rs:92`). Recordability audit, grounded per table:

| State | Recorded? | Why / how |
|---|---|---|
| `ArithCache::Specialized{kind}` (`adapt.rs:84`) | **yes** — `(op_off, kind_tag)` for sites that *ended the training run* specialized | the 4-variant `ArithKind` is a pure tag; the runtime fast path re-guards operand kinds on every execution and deopts on miss (`adapt.rs` module invariant) |
| `InlineCache::Mono`/`Poly` (`ic.rs:55`) | **yes** — `(op_off, [shape key-list…])` (1 list for Mono, ≤`POLY_MAX` for Poly) | shape **ids are per-Vm** and never serialized; the *key list* is the portable identity (§3.3). The cached **index is NOT recorded** — it is re-derived at seed time (§3.3, the soundness keystone) |
| `GlobalCache::Cached` builtin sites (`adapt.rs:177`) | **yes** — `(op_off)` only, a "this site resolved to a builtin" marker | the *value* is re-resolved at seed time from the live builtin table via the site's own name operand; the version guard stays |
| `GlobalCache::IndexBound` (`adapt.rs:190`) | **no** | the stable `IndexMap` index depends on the runtime *define order* of user globals — not version-stable across runs; the brief's "builtins only" call, confirmed in code |
| `MethodCache::Mono` (`ic.rs:147`) | **no (v1)** | it caches `Rc::as_ptr` class identity + a live `Cc<Closure>` — run-local pointer identity, non-serializable. It is also warmup-free (Cold→Mono on first call), so seeding buys ~one chain-walk per site. Recorded follow-up, not silent |
| `class_base_shapes` (`run.rs:5102`) | **no (v1)** | keyed by runtime class pointer; built once per class on first construction. The recorded shape key-lists (below) still pre-build the transition *tree* it walks |
| `ShapeRegistry` transitions | **implicitly** — the deduped key-list table (§3.2) is interned at seed time, pre-building the transition tree (`shape.rs:57`) |

### 3.2 Format — a self-described trailing section of the archive

```
… existing archive bytes (magic·version·manifest·module table, archive.rs §) …
section_magic:  b"ASPGO\0\0\0"          (8 bytes, distinct from ASCRIPTA/ASO\0/ASCRIPTB)
section_version: u16                     (the PGO section's OWN minor tag; drift ⇒ skip)
section_len:    u32                      (payload byte length — skippable without parsing)
payload:
  key_list_table: count:u32 · [ nkeys:u32 · [klen:u32 · key]× ]×      (deduped, index-referenced)
  module_count:   u32
  [ module_key:str · chunk_sha256:[u8;32] ·
      proto_count:u32 ·
      [ proto_path: depth:u8 · [u32]×depth ·          (index path through chunk.protos)
        arith:   n:u32 · [ off:u32 · kind:u8 ]× ·
        fields:  n:u32 · [ off:u32 · nlists:u8 · [list_idx:u32]× ]× ·
        globals: n:u32 · [ off:u32 ]× ] ]×
```

Decode is hostile-input-safe in the `archive.rs` style: every length bounds-checked against
remaining input, counts clamped before allocation, **any** anomaly (bad magic where a section
was expected, unknown `section_version`, truncation, out-of-range list index) ⇒ the section is
**ignored and the program warms normally** — never a load failure. Unknown *other* trailing
sections (future magics) are skipped by `section_len`.

`build --pgo` is the only producer; absence of the flag = no section (the DBG `--strip`
precedent: opting *out* is just not opting in — `aso.rs:535`, "`ascript build` opts into debug
by default (`--strip` selects `with_debug = false`)"; PGO inverts the default, so no
post-hoc `--strip-pgo` rewriter is needed in v1 — rebuild without `--pgo`; a standalone
stripper is a recorded follow-up only if a real workflow demands it).

### 3.3 Seeding at load — the per-Vm remapping, precisely

At archive load (gated on `vm.specialize` and the `ASCRIPT_NO_PGO=1` kill switch being unset),
after each module's `from_bytes_verified`:

1. **Module binding:** the section's `chunk_sha256` must equal the sha256 of that module's
   stored chunk bytes; mismatch ⇒ skip that module's seeds entirely (the profile was recorded
   against different bytecode — offsets/proto paths would be meaningless).
2. **Shape interning:** each referenced key-list is interned through the *fresh* Vm's
   `ShapeRegistry::shape_for` (`shape.rs:69`) → a fresh per-Vm id. The section stores key
   lists, never raw ids — ids have no cross-Vm meaning by construction (`shape.rs` module doc:
   the registry is per-VM).
3. **Field-IC seeding — the index is DERIVED, never trusted:** for a `fields` entry at `off`,
   the seeder reads the property **name from the chunk's own const-pool operand** at that
   `GET_PROP`/`SET_PROP` site (trusted, verified bytes), finds its position in the interned
   key list, and installs `Mono{shape: fresh_id, index: position}` (or builds a `Poly` from
   ≤`POLY_MAX` lists). If the name is absent from the key list, the entry is **skipped**. This
   closes the one hole where a hostile/stale profile could otherwise cause wrong *behavior*
   rather than a miss: a trusted-index seed with a lying index would pass the shape guard and
   read the wrong slot. With derivation, the IC invariant (`ic.rs` module doc: "(shape, name)
   always maps to the same index") is re-established locally at seed time.
4. **Arith seeding:** install `ArithCache::Specialized{kind}` at `off`. The kind byte is
   range-checked; unknown ⇒ skip.
5. **Global seeding:** for each `globals` offset, read the site's name operand; if (and only
   if) it resolves in the **live** builtin table, install
   `GlobalCache::Cached{value, current_version}`. User-named or unresolvable ⇒ skip.

### 3.4 Versioning decision (derived from the codec, as required)

**No `ARCHIVE_VERSION` bump, no `ASO_FORMAT_VERSION` bump.** Grounds:

- `ModuleArchive::decode` verifiably ignores trailing bytes (it returns after the module
  table with no `pos == len` check). The PGO section appended after the module table is
  therefore *already* forward-compatible with every shipped reader: an old runtime runs the
  archive and simply warms normally — exactly the required failure semantics. This tolerance
  is promoted from accident to **contract**: a pinned test asserts "a v1 reader decodes an
  archive with trailing sections and ignores them", and the codec doc-comment documents the
  trailing-sections rule. The new reader parses sections by magic and skips unknown ones.
- A bump would *cost* compatibility for nothing: `decode` rejects on strict version
  inequality (`archive.rs:255`), so bumping would make every new archive — even one *without*
  a PGO section — unreadable by current runtimes.
- The bare `.aso` format is untouched: a `--pgo` build **always emits a (possibly 1-module)
  `ASCRIPTA` archive**, because the archive container is the home of optional sections; the
  `ASO\0` stream has no section mechanism short of an `ASO_FORMAT_VERSION` bump (its v26
  debug flag is *inside* the versioned stream), and `run_verified_aso`'s magic routing
  (`lib.rs:1757`) already runs 1-module archives.
- The PGO section carries its **own** `section_version` minor tag, so *its* format can drift
  independently with ignore-and-warm-normally semantics, never failing a run.

### 3.5 The soundness argument (the spec's core)

**Seeding only pre-fills caches whose guards still verify at runtime; a wrong seed degrades to
a miss/deopt, never wrong behavior.** Per cache:

- An **arith** seed is consulted only behind the operand-kind guard the runtime fast path
  always applies (`adapt.rs` correctness invariant); a kind mismatch takes the generic
  `apply_binop` and deopts the site — the exact runtime-warmed miss path.
- A **field-IC** seed is consulted only behind the `recv.shape == cached_shape` compare; a
  fresh-Vm shape id can only equal the receiver's shape if the receiver's insertion-ordered
  key layout *is* the recorded key list, in which case the derived index is correct by the
  shape invariant. Any other receiver ⇒ integer-compare miss ⇒ generic lookup (which records
  over the seed, exactly as a warmed entry would be replaced).
- A **global** seed stores a value resolved from the *live* builtin table at seed time and
  remains behind the version guard (`adapt.rs:197`); builtins are immutable, and a version
  bump invalidates exactly as for a runtime-warmed entry.
- **Nothing else is seeded.** Method ICs and `IndexBound` — the two caches whose validity
  depends on run-local identity/order — are excluded by design (§3.1).

Therefore the reachable state space of a seeded Vm is a subset of the state space of a warmed
Vm: every seeded entry is an entry warmup *could* have produced, behind the same guard. The
differential (§5) and the adversarial-seed fuzz axis (junk seeds injected directly into the
side tables must still yield byte-identical output — guards absorb everything) prove it.

### 3.6 CLI & recording mode

`ascript build app.as --pgo [-- training-args…]`: compile the archive as today, then **run the
program once, for real** (the training run executes user code — side effects happen, caps
compose as for a normal run, output streams; documented loudly). After the run, harvest: walk
every chunk's side tables, reverse-walk the `ShapeRegistry` (a child→(parent, key) reverse map
added to `shape.rs`) to turn cached shape ids into key lists, and append the section. A
training run that panics still embeds what it warmed (partial profiles are fine — they are
hints). `--pgo` composes with `--native` (the archive *is* the footer payload — BNDL) for the
goal-perf headline: a warm-starting, sandboxed, tree-shaken single binary.

### 3.7 Honest expectations (stated, then measured)

At the **current** cache model the v1 win is **bounded and possibly small**: seeding saves at
most `WARMUP_THRESHOLD` (8) generic executions per arith site, one generic lookup per IC/global
site, and the shape-tree construction — i.e. it wins *warmup time* on short-lived CLI
invocations and first-request latency, while **steady-state is unchanged by construction** (the
caches converge to the same fixed point either way; the A/B gate asserts ≈1.0× steady-state).
The section's compounding value is as the **carrier** the evidence-gated DECODE/JIT specs
consume (the JIT reads exactly these caches as type feedback — JIT spec §3.2); building the
record/seed/verify machinery here, where it is cheap to prove, is the point. Both numbers
(cold-start delta, steady-state ≈1.0) are measured and reported in `bench/WARM_RESULTS.md` —
whatever they are.

---

## 4. Unit C — workflow durable-log group commit

### 4.1 The corrected baseline (see §0.1)

Today, per `run`/`resume`: events accumulate in memory (`DeterminismContext.events`,
`det.rs:250`); `finish_workflow` (`workflow.rs:500`) serializes the whole log and `write_log`
(`workflow.rs:759`) commits it atomically (temp → optional `sync_all` → rename → optional
dir-fsync). Durability facts of the shipped default (`"fsync"`):

- A **completed** `run`/`resume` is fully durable (atomic snapshot, file+dir fsync'd).
- A crash **mid-run** (kill -9 / power) persists **nothing** from the in-flight run; `resume`
  re-executes every activity from the top. (A *recoverable* error still reaches
  `finish_workflow` and flushes — only hard crashes lose the stream.)
- Cost: one `F_FULLFSYNC` + dir-fsync + temp-file churn per commit — the measured 96%.

### 4.2 The modes (extending the existing option, `workflow.rs:367`)

`run/resume(wf, input, { log, durability?, groupWindowMs?, groupMaxEvents? })` — the options
object is the **only** configuration surface (decided: no `ascript.toml` default — a global
knob that silently relaxes durability for code that didn't ask is exactly the "silent
relaxation" the pillar forbids; per-workflow is the right granularity, and it is where `log`
already lives).

| `durability` | write granularity | fsync policy | `kill -9` mid-run | power/kernel loss |
|---|---|---|---|---|
| `"fsync"` (default — **unchanged**) | whole-log snapshot at finish (temp+rename) | file `sync_all` + dir fsync per commit | loses the whole in-flight run; `resume` re-executes all activities | completed commits never lost |
| `"group"` (new) | **per-event append** (`write(2)` at each record point) | **coalesced**: fsync when ≥`groupMaxEvents` (default 128) unsynced records exist, or an append/finish occurs ≥`groupWindowMs` (default 50 ms) after the oldest unsynced record | **loses nothing** (records are in the OS page cache the moment the recording call returns) | loses at most the unsynced tail — bounded by the window while appending; the final tail after process exit rides the OS writeback horizon (seconds). `resume` re-executes exactly the lost suffix |
| `"buffered"` (existing — unchanged) | whole-log snapshot at finish | none (OS-asynchronous writeback) | loses the in-flight run | recent commits may be lost (OS-dependent) |

**Hardening (deliberate, tested):** an unknown `durability` string is a Tier-2 error. Today
`read_options` treats anything ≠ `"buffered"` as fsync (`workflow.rs:388`) — safe-direction
but silent; a typo like `"groop"` must not silently select a different durability class.
Both engines symmetric (shared stdlib).

### 4.3 Group mode — the appender

- **One chokepoint.** All `DetEvent` recording sites (`workflow.rs` ×3, `det.rs` ×11,
  `stdlib/mod.rs:782`) are refactored through a single `DeterminismContext::record_event`
  method (pure refactor for the existing modes), which — when a group appender is installed —
  also **pumps**: serializes `events[persisted..]` to newline-JSON records and issues **one
  `write(2)`** for the batch. **REPLAY coordination (reciprocal — REPLAY's front matter carries
  the shared rule):** REPLAY (`2026-06-12-record-replay-design.md`) needs the SAME
  `record_event` chokepoint for its trace capture; whichever spec merges first introduces the
  refactor, and the other rebases onto it. No user-space buffering survives a pump return: after any
  recording call returns to workflow code, its record is in the OS page cache (the kill-9
  guarantee). The appender (an open `File` + `persisted: usize` + `unsynced_since:
  Option<Instant>` + counters) lives beside the determinism cell for the duration of the
  `run`/`resume`; writes are synchronous (no borrow across `.await` — the write happens inside
  the recording call, exactly where the `Vec::push` happens today).
- **Deadline coalescing, pump-driven (no background thread in v1):** each pump (and the finish
  pump) checks `unsynced ≥ groupMaxEvents || oldest_unsynced_age ≥ groupWindowMs` and then
  `sync_all`s. Finish does **not** force an unconditional fsync (that would reinstate
  per-commit `F_FULLFSYNC` and forfeit the measured win); the contract is the table above —
  the window bound holds *while the process lives and appends*, and the final tail's
  power-loss exposure is the OS writeback horizon. This is the Redis `appendfsync everysec`
  durability class, stated plainly in the docs.
- **Resume under group:** open the existing log, **repair** (§4.4), parse via `log_to_events`,
  seed the replay cursor; `persisted` starts at the parsed-record count so only *new* events
  append. The `WorkflowCompleted` terminal record is appended (not snapshot-rewritten) at
  finish.
- **Why no fsync barrier before activities (the brief's barrier question, decided):**
  activity results returning to user code are **not** externally visible in the durability
  sense — replay re-derives them. The genuine external effect is the activity's *side effect
  itself*, and AScript activities are **at-least-once by construction**: on resume, a missing
  `ActivityCompleted` at the cursor switches the context to Record and **re-executes** the
  activity (`ctx_call_activity`, `workflow.rs:628-632`; same for `sleep`'s missing-`TimerSet`
  crash point, `workflow.rs:570-575`). Losing the unsynced tail therefore costs *re-execution
  of exactly those activities* — Temporal's at-least-once activity semantics, which the
  shipped engine already implements and the default mode already exhibits (with a *larger*
  re-execution set!). A pre-activity fsync barrier would buy a guarantee the model doesn't
  claim (exactly-once side effects) at the full per-event-fsync price. **The v1 barrier set is
  empty**; the docs state the at-least-once contract and the idempotent-activity guidance that
  goes with it (as Temporal's do).

### 4.4 Framing — torn-tail detection & repair (extending minimally)

Current framing: one JSON record per line, `seq`-numbered (`events_to_log`,
`workflow.rs:133`); the reader best-effort-skips malformed lines (`log_to_events`,
`workflow.rs:240`) — safe under the rename model, which can never tear. Appends *can* tear
(partial final `write` on power loss; out-of-order page writeback in theory). Minimal
extension:

- **Appended records carry a `"crc"` field** — CRC32 of the record's canonical JSON bytes
  with the crc field absent — keeping the log human-inspectable newline-JSON (no binary
  length-prefix framing; decided: the existing format is extended, not replaced).
- **Open-for-resume repairs by prefix-truncation:** scan line by line; the valid prefix ends
  at the first line that is (a) not newline-terminated, (b) not valid JSON, (c) crc-carrying
  with a failing crc, or (d) `seq`-discontinuous with its predecessor. The file is physically
  truncated to the end of the valid prefix before parsing — replay correctness requires a
  contiguous event *prefix*, so "skip and continue" (today's reader behavior) would be wrong
  for a torn append log; repair happens at open, the parser itself stays unchanged for legacy
  (rename-written, crc-less) logs. The truncation-point property battery (§5) proves every
  possible tear recovers.

### 4.5 Failure modes

| Failure | `"fsync"` | `"group"` |
|---|---|---|
| kill -9 mid-run | whole in-flight run re-executed on resume (today, now documented) | only un-pumped work (none — pump is synchronous in the recording call) — replay from the full log; **no re-execution beyond the in-flight activity itself** |
| power loss mid-run | as above | unsynced tail (≤ window/`maxEvents`) re-executed on resume |
| power loss after finish | nothing lost | completion record may be in the tail → a later `resume` misses the idempotent short-circuit (`completed_result`, `workflow.rs:723`) and re-runs, re-executing un-persisted activities — same at-least-once envelope |
| torn final append | n/a (rename is atomic) | repaired by prefix-truncation at open (§4.4) |
| `ENOSPC`/`EIO` on write or fsync | surfaced as a Tier-2 error (today's `write_log` posture: never rename-and-lie) | same — a failed pump/fsync surfaces; the workflow does not continue believing itself durable |
| disk full during repair-truncate | truncation only shrinks; an `ftruncate` failure surfaces as a clean error before replay |

---

## 5. Correctness — the gating invariants

Gates 1–14 (`goal.md`) + 15–18 (`goal-perf.md`) verbatim, plus per-unit:

- **A — the stale-hit adversarial battery** (each as a spawn test against the real binary,
  cold cache per case): edit the entry → miss; edit each *transitive* module (incl. a
  path-dep package module) → miss; same content at a different path → miss (§2.4, asserted
  with a panic-producing program whose diagnostic must show the *invoking* path); `touch`
  (mtime-only) → hit; flag change (`debug`/`shake` key entries perturbed via the test seam) →
  miss; simulated `ASO_FORMAT_VERSION`/key-schema change → miss; lockfile/package-map change →
  miss; **corrupted artifact** (bit-flipped) → verifier rejects → recompile + slot repaired →
  correct output; corrupted manifest → miss; **concurrent runs racing one key** → both
  byte-identical output, slot intact. Plus the identity gate: cached vs uncached run
  byte-identical over the multi-module corpus **including panic stderr** (the diagnostics
  parity test) and worker-spawning programs (the archive worker-parity path).
- **B — differential mode + fuzz axis (Gate 15):** (a) recorded-then-seeded vs unseeded
  byte-identical over the corpus + goldens, both feature configs; (b) the **adversarial seed
  axis** — a harness injecting *arbitrary* (offset, kind/key-list/builtin-marker) junk
  directly into the side tables before running fuzz-generated programs, asserting
  byte-identity (guards must absorb every lie — this tests the §3.5 argument itself, not just
  well-formed profiles); (c) **coverage assertion** (anti-false-green): seeding the training
  program's own archive installs >0 entries, and after re-running the training input the
  seeded arith sites are *still* `Specialized` (guards held — the seeds were live, not dead
  weight); a sabotage check (mis-keyed digest ⇒ 0 installs ⇒ the coverage test fails) proves
  the tripwire trips. (d) hostile-section fuzzing of the PGO decoder alongside the archive
  fuzz target.
- **C — the crash-recovery battery** (real `kill -9` via the `tests/cli.rs` spawn precedent):
  a workflow whose activities append markers to a side file and which signals (marker file)
  after activity k of n; the parent kills -9 at the signal, then `resume`s in a fresh
  process. Assert per mode: `"fsync"` — activities 1..k re-execute (marker counts double),
  final result correct; `"group"` — activities 1..k replay (markers do **not** double), k+1..n
  execute, result correct, log never torn. Plus the **truncation property battery**: a valid
  group log truncated at *every byte offset* must repair + resume to the correct final result.
  Plus mode-orthogonality: all three modes produce byte-identical *program output* on a
  crash-free run (the differential corpus carries a workflow program).
- **All units:** kill-switchable (`--no-cache`/`ASCRIPT_NO_COMPILE_CACHE`, `ASCRIPT_NO_PGO`,
  `durability` default unchanged); clippy + tests green in both feature configs (workflow is
  feature-gated — the no-default-features build compiles all units' core seams); no borrow
  across `.await`; docs per Gate 13 (§7-listed pages).

## 6. Performance — honest, same-session A/B (Gates 16–18)

All numbers recorded in `bench/WARM_RESULTS.md`; baseline and candidate are one binary pair
measured interleaved in one session (`bench/ab.sh` once LANE's harness lands, else the same
protocol inline); peak RSS reported alongside (Gate 18).

- **A:** cold vs warm `ascript run` wall-time on (i) a real multi-module example and (ii) a
  generated N-module tree (`bench/gen_module_tree.py`, committed; N ∈ {10, 100, 500}). The
  expected win scales with project size (the entire front-end is skipped); the *hit-path
  overhead floor* (hashing the file set) is reported too. Also: miss-path overhead vs no-cache
  baseline (the publish cost) must be within noise of one extra archive write.
- **B:** cold-start delta on a short-lived seeded CLI workload + a first-N-requests
  server-shaped workload (warmup-window latency), seeded vs unseeded, same archive; AND
  steady-state A/B over the bench corpus asserting ≈1.0× (no seeding tax, no regression when
  the section is absent — the zero-cost-when-off proof for the loader path). Honest framing
  per §3.7.
- **C:** `workflow_loop` (and a long single-workflow many-activity variant, added to
  `bench/profiling/`) under `"fsync"` / `"group"` / `"buffered"`. Expectation (stated, not
  promised): order-of-magnitude on group for the fsync-dominated shape, since the 82%
  `F_FULLFSYNC` slice collapses to ~window-rate; `"fsync"` numbers must be **unchanged** vs
  baseline (the default pays nothing for the feature's existence — Gate 12/17 posture).

## 7. Scope & rejected alternatives

**In scope:** everything above. **Out of scope / rejected (recorded so they aren't
re-litigated):**

- **Persisting JIT/native code in the cache or the PGO section** — rejected: JIT-spec §6.2
  territory ("the `.aso` is NOT JIT output"), and no JIT exists. The PGO section carries
  *interpreter cache* state only.
- **Auto-tuned durability** (adaptive window sizing, fsync-latency feedback) — rejected v1;
  the explicit three-mode contract ships first. Auto-GC for the compile cache — recorded
  follow-up (§2.7), `cache clean` is the v1 lever.
- **Sharing the compile cache across machines / a remote cache** — rejected: local cache
  only; the key deliberately includes a local binary stamp (§2.3) and a local canonical entry
  path (§2.4), neither of which is portable. Distribution artifacts are `ascript build`'s job.
- **PGO profile merging from multiple training runs** — recorded follow-up; v1 is one
  training run per `--pgo` build (the section format's per-module records do not preclude a
  future merger).
- **Seeding `MethodCache` / `IndexBound` / `class_base_shapes`** — excluded by the soundness
  audit (§3.1); method-IC seeding recorded as follow-up if DECODE/JIT-era evidence wants it.
- **A fourth `"async"` durability mode (background fsync thread)** — folded: `"buffered"`
  already occupies the no-explicit-fsync tier with OS-asynchronous writeback; a dedicated
  fsync thread would add a cross-thread seam to the `!Send` runtime for a tier between
  `"group"` (bounded window) and `"buffered"` (OS horizon) that no measured workload demands.
- **A background deadline-flusher task for group mode** — pump-driven v1 (§4.3); revisit only
  if a real workload shows long event-free gaps where the window bound matters.
- **`ascript.toml [workflow]` durability defaults** — rejected (§4.2): silent relaxation
  vector.
- **Caching for `--tree-walker`/`--inspect`/`--profile`/test/REPL paths** — deliberately
  uncached (§2.8), documented.
- **Binary length-prefix log framing** — rejected for v1: newline-JSON + crc + prefix-repair
  keeps the log human-inspectable and the reader backward-compatible (§4.4).

## 8. Grounding (verified, file:line, 2026-06-12)

- `src/lib.rs:2024` `run_file_on_vm_with_packages` (the `run` path: read → `compile_source`
  at `:2040` → run; per-import compiles at runtime); `:1744` `run_verified_aso` magic routing
  (`:1757` `ASCRIPTA` dispatch); `:1808` `run_verified_archive` (caps intersection `:1841`,
  worker archive parity `:1858-1863`); `:1074` `compile_archive` /
  `:1095` `compile_archive_with_shake` (the shake toggle seam).
- `src/vm/aso.rs:167` `ASO_FORMAT_VERSION = 27`; `:149-156` + `:536-556` the v26 optional
  strippable debug section (flag-byte precedent); `:780-783` side tables asserted empty at
  serialization (caches are never serialized today).
- `src/vm/archive.rs:27` `ARCHIVE_VERSION = 1`; `:224-241` `encode` (exact fields, no
  section framing); `decode` returns after the module table **without** a trailing-bytes
  check (the §3.4 tolerance); `:255` strict version equality; `join_logical` the shared
  key convention.
- `src/pkg/cache.rs:26` `cache_root` (`$ASCRIPT_CACHE` first); `:89` `store_dir`; `:100`
  `tmp_dir` (staging-then-atomic-move precedent); `src/pkg/hash.rs:23` the `asum1-`
  algorithm-versioning prefix (the key-schema-prefix precedent), `:30` fail-closed tree hash.
- `src/vm/chunk.rs:421-435` the four offset-keyed side tables; `:794-853` their accessors.
- `src/vm/adapt.rs:44` `WARMUP_THRESHOLD = 8`; `:49-62` `ArithKind` (4 variants); `:84`
  `Specialized`; module-doc correctness invariant (guard-then-fast-path, deopt on miss);
  `:170-224` `GlobalCache` (`Cached` version guard `:197`; `IndexBound` define-order
  dependence `:181-190`).
- `src/vm/ic.rs:48` `POLY_MAX = 4`; `:55-69` `InlineCache` states; module-doc invariant
  "(shape, name) always maps to the same index"; `:140-152` `MethodCache::Mono` =
  `Rc::as_ptr` identity + live `Cc<Closure>` (non-serializable).
- `src/vm/shape.rs:28-78` `ShapeRegistry` (per-Vm, transition tree, `shape_for`);
  `src/vm/run.rs:87` `shapes`, `:92`/`:5102` `class_base_shapes` (runtime-ptr-keyed),
  `:2327` literal shape assignment, `:5384` `resync_object_shape`.
- `src/stdlib/workflow.rs:367-389` `read_options` (`durability: "fsync"|"buffered"`, unknown
  ⇒ silently fsync — the §4.2 hardening target); `:500-526` `finish_workflow` (the ONLY
  `write_log` caller, `:524` — whole-log-per-commit confirmed by grep); `:759-793` `write_log`
  (temp → `sync_all` → rename → dir-fsync; pid-qualified temp; single-writer contract);
  `:628-632` + `:570-575` the crash-point switch-to-Record (at-least-once re-execution is
  shipped behavior); `:133` `events_to_log` (newline-JSON, `seq`); `:240` `log_to_events`
  (best-effort skip — the §4.4 prefix-repair target); `:723` `completed_result`.
- `src/det.rs:250` `DeterminismContext.events`; eleven `events.push` sites at
  `:288,313,332,346,359,381,397,415,458,484,544` + `src/stdlib/mod.rs:782` (the §4.3
  chokepoint refactor inventory).
- `bench/PROFILING_RESULTS.md` — workflow_loop: fsync 96% (`F_FULLFSYNC` 82%, unlink 8%,
  open 4%); "the lever is durability engineering … a `buffered` durability mode";
  `bench/profiling/workflow_loop.as` — 3 000 × `run()` with per-iteration log removal.
- `tests/cli.rs` — the spawn-the-real-binary test precedent (`Command::new(bin)`), reused for
  the kill-9 battery and the cache adversarial battery.
- `src/main.rs:14-100` the `Run`/`Build` clap surface (`--strip`, `--native`, `CapFlags`,
  trailing var-args) — the home of `--no-cache`, `--pgo`, and the `cache` subcommand.
- Format exemplars followed: `superpowers/specs/2026-06-08-baseline-jit-design.md` (rigor
  model; its §3.2 names these same caches as the JIT's feedback — the §3.7 carrier claim),
  `superpowers/specs/2026-06-11-self-contained-bundles-design.md` (archive layout, §9
  execution standards inherited by the plan).
- External precedents: ccache/sccache direct-mode manifests (§2.5); Redis `appendfsync
  everysec` (§4.3's durability class); Temporal at-least-once activities (§4.3); PEP 659
  (the cache model being seeded).
