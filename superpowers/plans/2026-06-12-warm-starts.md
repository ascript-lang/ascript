# Warm Starts & Durable-Log Throughput (WARM) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. A final **holistic
> review** covers the whole branch before merge. A task is closed only when every box under it
> is ticked.

**Goal:** Three independent units. **A** — a content-addressed compile cache so `ascript run`
skips parse/resolve/compile when nothing changed (a stale hit is a Gate-1 wrong-code bug; the
adversarial battery is the core deliverable). **B** — `ascript build --pgo` records warmed
arith/IC/global/shape state into an optional, strippable, version-tagged **trailing section**
of the BNDL archive; the loader pre-seeds the same side tables behind the same guards
(seeded == unseeded byte-identical, a new differential mode + fuzz axis). **C** — workflow-log
durability becomes an explicit policy: keep `"fsync"` (default, unchanged) and `"buffered"`,
add `"group"` (per-event appends + coalesced fsync + torn-tail repair) with a precise
at-least-once loss-window contract and a real `kill -9` crash battery.

**Architecture:** Spec: `superpowers/specs/2026-06-12-warm-starts-design.md` — **read it fully
before any task**; §0's three code-vs-brief corrections are load-bearing. Unit A: a two-level
(location-key + manifest re-hash) cache under `$ASCRIPT_CACHE/compiled/`, artifact = an
**unshaken, debug-carrying, neutral-caps 1..N-module archive** run through the existing
`run_verified_aso` magic routing; fail-open everywhere, verify-on-hit, atomic publish. Unit B:
a self-described trailing section (`ASPGO\0\0\0` · u16 version · u32 len · payload) appended
after the module table — **no `ARCHIVE_VERSION`/`ASO_FORMAT_VERSION` bump** (the decoder's
trailing-byte tolerance is pinned as contract first); seeds store shape **key lists** (ids are
per-Vm) and **never** carry field indices (derived at seed time from the chunk's own const
operands). Unit C: a single `DeterminismContext::record_event` chokepoint + a group appender
(`write(2)` per pump, crc'd newline-JSON records, prefix-truncation repair at open,
deadline-coalesced fsync).

**Tech stack:** Rust, single binary `ascript`. Touched: `src/lib.rs`, `src/main.rs`,
`src/vm/archive.rs`, `src/vm/{chunk,shape,run}.rs` (Unit B seams), `src/pkg/cache.rs`
(namespace only), `src/stdlib/workflow.rs`, `src/det.rs`, `src/stdlib/mod.rs` (one push site),
`tests/{cli,vm_differential,native}.rs` + new `tests/{compile_cache,pgo,workflow_durability}.rs`,
`fuzz/fuzz_targets/`, `bench/`, `docs/content/`, `examples/`. New deps: `crc32fast` (tiny,
pure-Rust, for Unit C record crc — confirm with owner if a hand-rolled CRC32 is preferred over
the dep; either is acceptable, the wire format is the contract).

**Binding execution standards (non-negotiable):**
- TDD per task: failing test → minimal code → green → commit. Frequent commits, house trailer on
  every commit: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Production-grade mandate (goal.md Gates 1–14, goal-perf.md 15–18):** any bug found while
  working — ours or pre-existing, direct or incidental — is fixed **in this branch** with a
  failing-test-first regression guard. No placeholders, no silent deferrals; every §7 spec
  rejection stays rejected, every follow-up is recorded in `roadmap.md`.
- Byte-identity is never relaxed: cached vs uncached, seeded vs unseeded, and every durability
  mode (crash-free) must be byte-identical to today's behavior. Fix the unit, never the
  assertion.
- Clippy clean AND tests green under `--all-targets` and `--no-default-features --all-targets`
  before any "done" claim. Evidence (command output) before assertions.
- No `RefCell`/resource borrow across `.await`; cache/PGO/log readers parse UNTRUSTED bytes —
  every length bounds-checked, no reachable `unwrap`/`panic!` from hostile input.
- Branch: `feat/warm-starts` off `main`. Merge `--no-ff` after holistic review.

---

## File Structure

**New files:**
- `src/cache/compile_cache.rs` (+ `src/cache/mod.rs`) — CLI-side compile cache: key struct,
  manifest codec, lookup/publish/clean. Lives OUTSIDE the core runtime (the SP6 posture:
  TOML/IO stays out of the engine); core sees nothing.
- `src/vm/pgo.rs` — the PGO section codec (encode/decode, hostile-safe), the recorder
  (harvest), and the seeder (remap + install).
- `tests/compile_cache.rs`, `tests/pgo.rs`, `tests/workflow_durability.rs` — the three
  unit batteries (spawn-based where the spec demands real processes).
- `bench/gen_module_tree.py`, `bench/run_warm_bench.sh`, `bench/WARM_RESULTS.md`.
- `bench/profiling/workflow_long.as` — single long workflow, many activities (the per-event
  shape `workflow_loop` doesn't cover).
- `examples/compile_cache/{main.as,util.as,model.as}` — multi-module cache demo corpus entry.
- `examples/advanced/workflow_durability.as` — production-shaped durability-modes example.
- `fuzz/fuzz_targets/pgo_section.rs` — hostile PGO-section bytes.

**Modified files:**
- `src/lib.rs` — cached-run wiring, `--pgo` build path, training-run entry.
- `src/main.rs` — `--no-cache`, `Cache { Clean, Dir }` subcommand, `--pgo` flag.
- `src/vm/archive.rs` — pinned trailing-sections contract + section append/scan helpers.
- `src/vm/shape.rs` — reverse walk (`keys_of`); `src/vm/chunk.rs` — side-table iteration for
  harvest; `src/vm/run.rs` — seed install entry (gated on `specialize`).
- `src/stdlib/workflow.rs` — `Durability` enum, group appender, repair-on-open, option
  hardening; `src/det.rs` + `src/stdlib/mod.rs` — `record_event` chokepoint.
- `tests/vm_differential.rs` — PGO differential mode; `fuzz/fuzz_targets/differential.rs` —
  adversarial-seed axis; `tests/native.rs` — `--pgo --native` parity.
- `docs/content/{cli,runtime}.md`, `docs/content/stdlib/workflow.md` (existing pages — **no
  NAV change needed**; confirm and record per Gate 13).
- `CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md` — status updates (final task).

---

## Task 0: Pin the assumed behaviors (archive trailing tolerance + workflow baseline)

The spec leans on two currently-undocumented behaviors. Pin them FIRST so any drift fails
loudly before we build on it.

**Files:** `src/vm/archive.rs` (tests + doc-comment), `src/stdlib/workflow.rs` (test)

- [ ] **Step 1: Write the pinning tests** (in `archive.rs`'s `mod tests`):

```rust
/// WARM §3.4 — the trailing-sections CONTRACT. A v1 reader must DECODE an archive
/// that carries trailing bytes after the module table and IGNORE them (this is what
/// makes the PGO section forward-compatible with already-shipped runtimes without an
/// ARCHIVE_VERSION bump). This was implicit; WARM makes it contractual.
#[test]
fn decode_ignores_trailing_sections() {
    let arch = ModuleArchive::new(
        0,
        CapSet::all_granted(),
        [0u8; 32],
        vec![("main.as".to_string(), vec![1, 2, 3])],
    );
    let mut bytes = arch.encode();
    // A future self-described section: magic(8) · version(u16) · len(u32) · payload.
    bytes.extend_from_slice(b"ASPGO\0\0\0");
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let back = ModuleArchive::decode(&bytes).expect("trailing sections must be ignored");
    assert_eq!(back, arch);
    // Garbage trailing bytes (not even a section frame) are equally ignored.
    let mut junk = arch.encode();
    junk.extend_from_slice(&[0xFF; 7]);
    assert_eq!(ModuleArchive::decode(&junk).expect("junk tail ignored"), arch);
}
```

  And in `workflow.rs` (the §0.1 baseline, so Unit C's claims are test-anchored):

```rust
/// WARM §0.1/§4.1 baseline pin: the log is written ONCE per run (at finish), not
/// per event — a mid-run crash today persists nothing. This test documents the
/// shipped contract Unit C's `"group"` mode improves on; if someone later adds an
/// incremental write to the default path, this trips and the spec table is revisited.
#[tokio::test]
async fn fsync_mode_writes_nothing_until_finish() { /* run a workflow whose activity
    asserts !log_path.exists() mid-run via std::fs, then assert the complete log
    exists after run() returns — implementer wires it through run_source/capture */ }
```

- [ ] **Step 2: Run — expect PASS** (these pin EXISTING behavior; if either FAILS, stop:
  the spec's ground truth is wrong — escalate before proceeding).
- [ ] **Step 3:** Promote the tolerance to documentation: extend the `ModuleArchive::encode`/
  `decode` doc-comments with the trailing-sections rule (zero or more self-described trailing
  sections; unknown ⇒ skip; readers MUST NOT require `pos == len`).
- [ ] **Step 4: Commit** — `test(archive/workflow): pin trailing-section tolerance + once-per-finish log write (WARM Task 0)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer confirms both tests pass on a clean checkout of the
  branch point, and that NO production code changed in this task.

## Task 1 (A): Module-graph walk reuse + `CompileCacheKey` + manifest codec

**Files:** Create `src/cache/mod.rs`, `src/cache/compile_cache.rs`; modify `src/lib.rs`
(extract/expose the walk)

- [ ] **Step 1: Extract the reachability walk.** Read `compile_archive_with_shake`
  (`src/lib.rs:1095-1330`) and factor its module-graph enumeration into a shared helper the
  cache keyer AND `compile_archive` both call (so the keyed set and the compiled set cannot
  drift — spec §2.5):

```rust
/// One reachable module of the entry's import graph, as the BNDL walk discovers it.
pub struct GraphModule {
    pub logical_key: String,    // the join_logical key (archive.rs convention)
    pub path: std::path::PathBuf, // canonical on-disk path (the dedup identity)
    pub source: String,         // the file bytes as read (hashed by the cache)
}

/// Walk the import graph from `entry` (the same enumeration compile_archive uses),
/// WITHOUT compiling. Errors fail OPEN at the cache layer (the caller runs uncached).
pub fn collect_module_graph(entry: &Path) -> Result<Vec<GraphModule>, AsError>;
```

  This is a refactor of existing walk code, not new graph logic — `compile_archive` is
  re-expressed over it and the WHOLE suite must stay green (the BNDL tests are the guard).
- [ ] **Step 2: Write the failing unit tests** for the key + manifest (in
  `src/cache/compile_cache.rs` `mod tests`): key canonicalization (flag order irrelevant —
  sorted; schema tag present; distinct on each field perturbation incl. entry path and
  package-map digest); manifest JSON round-trip; `validate_manifest` returns `Hit` only when
  every listed file re-hashes equal AND the artifact digest matches (cover: edited file,
  deleted file, mtime-only touch ⇒ still `Hit`, extra unrelated file ⇒ still `Hit`).
- [ ] **Step 3: Implement** `CompileCacheKey` exactly as spec §2.2 (serialize canonically:
  field-tagged, length-prefixed, flags sorted; `location_key() -> String` =
  hex(sha256(serialized)) prefixed `ck1-` — mirroring `asum1-`), `BinaryStamp::current()`
  (CARGO_PKG_VERSION + `current_exe()` len + mtime; any stat error ⇒ a `Disabled` stamp that
  the caller treats as cache-off — fail open), the manifest type
  `{ modules: Vec<{logical_key, path, sha256}>, artifact_sha256, created_unix_ms }` with
  serde_json codec, and `package_map_digest(&PackageMap)` (canonical sorted serialization).
- [ ] **Step 4: Run — expect PASS**; clippy both configs (`src/cache` must compile under
  `--no-default-features` — it is CLI-side, gate any `pkg`-feature types accordingly; if
  `PackageMap` is core (it is — `Interp.package_resolver`), no gating needed: verify).
- [ ] **Step 5: Commit** — `feat(cache): module-graph walk extraction + CompileCacheKey/manifest codec (WARM A §2.2-2.5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer diffs `compile_archive` before/after the extraction
  (behavior-preserving), re-runs `cargo test --test native` + the BNDL archive tests, and
  probes the key: two keys differing only in flag ORDER must collide (canonical), differing
  in any VALUE must not.

## Task 2 (A): Cache store — lookup, atomic publish, verify-on-hit, `ascript cache` CLI

**Files:** `src/cache/compile_cache.rs`, `src/main.rs`, `tests/compile_cache.rs`

- [ ] **Step 1: Write the failing tests** (in `tests/compile_cache.rs`, hermetic — every test
  sets a unique `ASCRIPT_CACHE` tempdir; spawn the real binary per the `tests/cli.rs`
  precedent):

```rust
// (helpers: write a 3-module program {main.as imports ./util.as imports ./model.as},
//  bin() = env!("CARGO_BIN_EXE_ascript"), run(dir, args...) -> Output)

#[test]
fn second_run_hits_and_is_byte_identical() {
    // cold run → output O1, cache slot exists (manifest + program.aso);
    // warm run → output O2 == O1 (stdout AND stderr AND exit code).
}

#[test]
fn corrupted_artifact_fails_closed_to_recompile_and_repairs() {
    // cold run; bit-flip a byte mid-program.aso; warm run → output STILL correct
    // (verifier rejected → recompiled), and the slot afterwards re-validates clean.
}

#[test]
fn cache_clean_removes_compiled_namespace_only() {
    // seed a fake pkg store entry + run once; `ascript cache clean` → compiled/ gone,
    // store/ intact; `ascript cache dir` prints the root.
}

#[test]
fn concurrent_runs_racing_one_key_both_succeed() {
    // spawn N=4 simultaneous cold runs of the same program; all four outputs equal;
    // slot afterwards valid (atomic last-writer-wins).
}
```

- [ ] **Step 2: Run — expect FAIL** (no store, no CLI).
- [ ] **Step 3: Implement the store** (spec §2.7): `lookup(key) -> Option<Verified>` (read
  manifest under `cache_root()/compiled/<ck1-…>/`, re-hash listed files + artifact; any
  anomaly ⇒ `None`); `publish(key, manifest, artifact_bytes)` (stage in `cache_root()/tmp/`
  — `pkg::cache::tmp_dir()` — write `program.aso`, then `manifest.json`, then TWO renames into
  the slot with the **manifest renamed last**; create dirs as needed; any IO error ⇒ `Err`
  the caller swallows as fail-open). Implement `cache clean` (remove `compiled/` recursively,
  print a count) + `cache dir` in `src/main.rs` as a new `Command::Cache { Clean, Dir }`
  subcommand.
- [ ] **Step 4: Run — expect PASS** (the hit test needs Task 3's wiring for the cached RUN —
  structure the test file so store-level tests pass now and the spawn tests are added/enabled
  in Task 3 if needed; do NOT mark anything ignored without a tracking note).
- [ ] **Step 5: Commit** — `feat(cache): compiled/ store — atomic publish, verify-on-hit, cache clean/dir CLI (WARM A §2.7)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer kills a publish mid-stage (simulate: pre-create the
  slot dir read-only) and confirms fail-open; confirms `clean` cannot escape `compiled/`
  (path construction audit); confirms the manifest-last rename ordering in the code.

## Task 3 (A): Wire the cached run path + the adversarial invalidation battery

**Files:** `src/lib.rs`, `src/main.rs`, `tests/compile_cache.rs`

- [ ] **Step 1: Write the failing battery** (spec §5-A, each a spawn test, cold cache per
  case): `edit_entry_misses`, `edit_transitive_module_misses` (each module of the 3-file
  program in turn), `edit_path_dep_package_module_misses` (a `{path=…}` dep),
  `same_content_different_path_misses_and_diagnostics_show_invoking_path` (a panicking
  program copied to two dirs: each run's stderr caret must name ITS OWN path),
  `touch_without_change_hits`, `flag_change_misses` (via the `#[doc(hidden)]` key seam),
  `lockfile_change_misses`, `no_cache_flag_and_env_bypass`
  (`--no-cache` / `ASCRIPT_NO_COMPILE_CACHE=1` ⇒ no slot created),
  `tree_walker_inspect_profile_paths_uncached`, `panic_output_parity`
  (cached vs uncached stderr byte-identical for a panicking multi-module program),
  `worker_program_parity` (a `worker fn` program: cached run == uncached run — the archive
  worker-shipping path), `aso_run_unaffected` (`run file.aso` never consults the cache).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement the wiring** in `src/lib.rs`:

```rust
/// WARM A: the cached `run` front door. Decides cacheability, looks up, and on
/// miss compiles via compile_archive_with_shake(entry, /*debug*/true, /*shake*/false)
/// with archive.caps = CapSet::all_granted() (the NEUTRAL floor — runtime caps
/// compose by intersection, spec §2.6), publishes, and runs the artifact through
/// run_verified_aso's EXISTING magic routing — hit and miss share one run path.
/// EVERY cache-layer error falls open to run_file_on_vm_with_packages (today's path).
pub async fn run_file_on_vm_cached(
    path: &Path,
    script_args: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    no_cache: bool,
) -> Result<i32, AsError>
```

  `src/main.rs`: add `--no-cache` to `Run`; route the plain `.as`+VM case through
  `run_file_on_vm_cached` (with `no_cache = flag || env`); `--tree-walker`/`--inspect`/
  `--profile` keep their existing routes untouched. **Diagnostics parity:** on a hit, after
  `from_bytes_verified`, re-bind the entry module source to the invoking path
  (`chunk.set_module_source` with the live `SourceInfo`) iff the parity test demands it —
  drive this from the failing test, not speculation.
- [ ] **Step 4: Run — expect PASS** on the full battery; then `cargo test` + clippy, BOTH
  configs; `cargo test --test vm_differential` both configs (the cache is CLI-side — the
  differential must be untouched; verify no diff to its files).
- [ ] **Step 5: Add the example corpus entry** `examples/compile_cache/{main.as,util.as,
  model.as}` (a small multi-module program with a comment block explaining the cache;
  runnable, exercised by the conformance corpus — verify with `target/release/ascript run`).
- [ ] **Step 6: Commit** — `feat(cache): cached ascript run — fail-open wiring + adversarial stale-hit battery (WARM A §2.1/§5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer **sabotage-tests the battery**: temporarily make
  `lookup` skip the manifest re-hash and confirm `edit_transitive_module_misses` FAILS (then
  revert) — the battery must actually detect stale hits, not just pass. Reviewer also
  measures one hit by eye (`time ascript run` cold vs warm on the example) and confirms the
  hit skips compilation (e.g. via a debug log line gated off in release, or strace-level
  evidence) — anti-false-green for the cache itself.

## Task 4 (A): Unit-A bench — generator, cold/warm A/B, report

**Files:** `bench/gen_module_tree.py`, `bench/run_warm_bench.sh`, `bench/WARM_RESULTS.md`

- [ ] **Step 1:** Write `bench/gen_module_tree.py` (deterministic: N modules, a chain+fan
  import graph, each module a few fns/classes; entry imports the roots; prints nothing but a
  checksum line so output is comparable) and `bench/run_warm_bench.sh` (same-session protocol:
  for N in 10 100 500 — generate, run cold ×5 / warm ×5 interleaved with `--no-cache` runs,
  report medians + the hit-path floor + `/usr/bin/time -l` peak RSS; plus the real
  `examples/compile_cache` case).
- [ ] **Step 2:** `cargo build --release`; run it; start `bench/WARM_RESULTS.md` with the
  machine/date/methodology header + the Unit-A table (cold ms, warm ms, speedup, miss-overhead
  vs `--no-cache`, RSS). Honest numbers; no target asserted — the report IS the deliverable.
- [ ] **Step 3: Commit** — `bench(warm): module-tree generator + cold/warm A/B report (WARM A, Gates 16/18)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer re-runs the script, confirms numbers within noise of
  the report, and that miss-overhead is within noise of one extra archive write (else the
  publish path has a bug to fix here).

## Task 5 (B): PGO section codec + trailing-section scan

**Files:** `src/vm/pgo.rs` (new), `src/vm/archive.rs`, `fuzz/fuzz_targets/pgo_section.rs`

- [ ] **Step 1: Write the failing tests** (in `src/vm/pgo.rs` `mod tests`): encode→decode
  round-trip of a representative `PgoSection` (key-list table dedup; modules with arith/
  fields/globals records; nested proto paths); decode of: truncation at every byte offset ⇒
  `None` (never panic — loop the offsets); unknown `section_version` ⇒ `None`; out-of-range
  key-list index ⇒ `None`; count bombs (huge declared counts, tiny buffer) ⇒ `None` with no
  large allocation (the `archive.rs` clamp discipline). Plus in `archive.rs`: a
  `scan_trailing_sections(bytes) -> Vec<(magic, version, payload)>` helper test — unknown
  magics skipped by length, a malformed frame ends the scan cleanly.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** the types + codec exactly per spec §3.2:

```rust
pub const PGO_SECTION_MAGIC: [u8; 8] = *b"ASPGO\0\0\0";
pub const PGO_SECTION_VERSION: u16 = 1;

pub struct PgoSection { pub key_lists: Vec<Vec<String>>, pub modules: Vec<PgoModule> }
pub struct PgoModule {
    pub module_key: String,
    pub chunk_sha256: [u8; 32],
    pub protos: Vec<PgoProto>,
}
pub struct PgoProto {
    pub path: Vec<u32>,                  // index path through chunk.protos (empty = root)
    pub arith: Vec<(u32, u8)>,           // (op_off, ArithKind tag)
    pub fields: Vec<(u32, Vec<u32>)>,    // (op_off, key-list indices) — NO field index!
    pub globals: Vec<u32>,               // builtin-resolved GET_GLOBAL offsets
}
// encode(&self) -> Vec<u8> (the section FRAME: magic·version·len·payload)
// decode(payload: &[u8]) -> Option<PgoSection>   — hostile-safe, None on ANY anomaly
```

  And `append_section(archive_bytes, frame)` / `scan_trailing_sections` in `archive.rs`
  (pure byte helpers; the `ModuleArchive` struct itself is untouched — sections ride OUTSIDE
  `encode`/`decode`, preserving Task 0's pin).
- [ ] **Step 4:** Add `fuzz/fuzz_targets/pgo_section.rs` (decode arbitrary bytes; assert
  no panic, bounded allocation); `cd fuzz && cargo build` proves it compiles.
- [ ] **Step 5: Run — expect PASS**; clippy both configs.
- [ ] **Step 6: Commit** — `feat(pgo): trailing-section frame + hostile-safe PGO codec (WARM B §3.2)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer runs the truncation loop with a large random section,
  runs the fuzz target ≥5 min where available, and confirms `fields` records carry NO index
  field anywhere in the wire format (the §3.3 soundness keystone is structural).

## Task 6 (B): Recorder — harvest + `ascript build --pgo`

**Files:** `src/vm/shape.rs`, `src/vm/chunk.rs`, `src/vm/run.rs`, `src/lib.rs`, `src/main.rs`,
`tests/pgo.rs`

- [ ] **Step 1: Write the failing tests:** (a) `shape.rs`: `keys_of(shape) -> Option<Vec<String>>`
  reverse walk (intern `["a","b","c"]`, reverse it; unknown id ⇒ `None`; EMPTY_SHAPE ⇒ `Some([])`);
  (b) `tests/pgo.rs` spawn test: `ascript build prog.as --pgo -o out.aso` on a program with a
  hot int loop + a hot monomorphic `o.x` read + a `math.abs` import call → the artifact is an
  `ASCRIPTA` archive (even single-module) whose trailing PGO section decodes with: ≥1
  `Specialized(Int)` arith record, ≥1 field record whose key list contains `"x"`, ≥1 global
  record; the training run's stdout appeared (a real run). And: `build` WITHOUT `--pgo` emits
  byte-identical output to pre-WARM `build` (no section, single-module programs still emit
  bare `ASO\0` — `git`-level guarantee via the existing build tests staying green).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement:**
  - `ShapeRegistry::keys_of` (maintain a reverse `child → (parent, key)` map filled in
    `add_key`; walk to root, reverse).
  - Harvest: `Vm::harvest_pgo(&self, modules: &[(String, Vec<u8>)]) -> PgoSection` — for each
    module chunk (decoded once for the walk), iterate proto tree; per chunk read the side
    tables (`field_ics`/`arith_caches`/`global_caches` borrows — plain sync code, no await);
    keep `ArithCache::Specialized`, `InlineCache::Mono/Poly` (ids → `keys_of` → deduped key
    lists; an id `keys_of` cannot resolve ⇒ skip that entry), `GlobalCache::Cached` sites;
    record `chunk_sha256` over the module's stored bytes. NOTE the harvest runs against the
    chunks the TRAINING Vm executed — wire the training run to run the archive's own decoded
    chunks (the `run_archive` test-seam pattern, captured output NOT required — stream live).
  - CLI: `--pgo` flag on `Build` (clap: `#[arg(long = "pgo")] pgo: bool` + trailing
    training args after `--` forwarded as the program's argv); `build --pgo` forces the
    archive container for single-module programs (spec §3.4); `lib.rs` gains
    `build_file_with_pgo(...)`: compile archive (shaken, debug — NORMAL build semantics; the
    PGO artifact IS a distribution artifact, unlike Unit A's cache entries) → training run →
    harvest → `append_section` → write.
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs (`--no-default-features`:
  the recorder is core VM code — must compile; the CLI flag is feature-independent).
- [ ] **Step 5: Commit** — `feat(pgo): warm-state harvest + ascript build --pgo (WARM B §3.1/§3.6)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer probes: a training run that PANICS still embeds a
  (partial) section; a program with a polymorphic-beyond-POLY_MAX site records NO field entry
  for it (Mega excluded); `--pgo --native` produces a runnable bundle (`tests/native.rs`
  extension); the side-table borrows are not held across any await (grep the harvest path).

## Task 7 (B): Seeder — remap, derive, install; kill switch; coverage assertion

**Files:** `src/vm/pgo.rs`, `src/vm/run.rs`, `src/lib.rs`, `tests/pgo.rs`

- [ ] **Step 1: Write the failing tests** (white-box, in `tests/pgo.rs` via `#[doc(hidden)]`
  seams):

```rust
#[tokio::test]
async fn seeding_installs_before_first_execution_and_guards_hold() {
    // Build --pgo an archive for the training program; load it with seeding via the
    // test seam; BEFORE running: the hot arith site's cache is Specialized(Int) and
    // the field site is Mono (white-box: inspect chunk side tables; seam returns
    // installed-count > 0 — the COVERAGE assertion). Run the training input: output
    // byte-identical to unseeded; AFTER running, the arith site is STILL Specialized
    // (guards held — the seed was live and valid, spec §5-B(c)).
}

#[tokio::test]
async fn digest_mismatch_skips_module_seeds_and_warms_normally() {
    // Corrupt one chunk_sha256 in the section: installed-count for that module = 0,
    // run output unchanged. (Sabotage proof that the coverage tripwire trips: with
    // ALL digests corrupted, the >0 assertion of the previous test would fail.)
}

#[tokio::test]
async fn derived_index_skips_absent_name() {
    // Hand-craft a section whose field key list does NOT contain the site's property
    // name: the entry is skipped (no install), output byte-identical.
}

#[tokio::test]
async fn kill_switch_and_generic_mode() {
    // ASCRIPT_NO_PGO=1 (seam-equivalent flag) ⇒ installed-count 0;
    // generic VM (no_specialize) ⇒ seeds skipped entirely; output identical in all modes.
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `seed_chunk(vm, chunk, &PgoModule, &key_lists) -> usize`
  (installed count) exactly per spec §3.3: digest check → shape interning via
  `vm.shapes.borrow_mut().shape_for(...)` (scoped borrow) → per-proto-path resolution
  (out-of-range path ⇒ skip proto) → arith install (`set_arith_cache`) with range-checked
  kind tag → field install with the **derived index** (read the site's name from the const
  operand — reuse the existing operand-decode helpers; name absent in key list ⇒ skip) →
  global install (name operand resolves in the live builtin table ⇒ `GlobalCache::set(value,
  current_version)`; else skip). Wire into the archive load path (`run_verified_archive` +
  the worker-isolate archive install), gated `vm.specialize && !ASCRIPT_NO_PGO`; thread an
  explicit `seed: bool` through the `#[doc(hidden)]` test entry points (env never read in
  parallel tests — the LANE Task-2 convention).
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs.
- [ ] **Step 5: Commit** — `feat(pgo): guard-verified seeding — per-Vm shape remap + derived indices + kill switch (WARM B §3.3/§3.5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer audits the three install paths against §3.5 line by
  line (every install lands behind an existing guard; NO code path trusts a profile index or
  bypasses a guard), greps for borrow-across-await in the seeder (must be all-sync), and
  hand-crafts one LYING profile (key list claims layout `[y,x]` for a site whose receivers
  are `[x,y]`) proving the run is byte-identical (shape miss → generic).

## Task 8 (B): Differential mode + adversarial-seed fuzz axis + Unit-B bench

**Files:** `tests/vm_differential.rs`, `fuzz/fuzz_targets/differential.rs`, `tests/property.rs`
(if present for generated batteries), `bench/run_warm_bench.sh`, `bench/WARM_RESULTS.md`,
`tests/pgo.rs`

- [ ] **Step 1: The differential mode (Gate 15).** Add to `tests/vm_differential.rs` a
  PGO projection over the runnable corpus: for each corpus program, build an archive, record
  a profile by running it once, then run **seeded** and assert byte-identity against the
  standard three modes. (Reuse the corpus enumeration + skip list; programs that can't build
  an archive — e.g. long-running server skips — follow the existing `EXAMPLE_SKIPS`.)
- [ ] **Step 2: The adversarial-seed axis.** In the fuzz differential target (and the in-suite
  generated battery): before running each generated program on the seeded mode, inject
  pseudo-random junk seeds (derived from the fuzz input — offsets, kinds, key lists) directly
  into the compiled chunk's side tables via the `#[doc(hidden)]` seam, then assert the output
  equals the unseeded modes. This fuzzes the GUARDS, not the codec (Task 5 fuzzes the codec).
  `cd fuzz && cargo build` must pass.
- [ ] **Step 3: Bench.** Extend `bench/run_warm_bench.sh`: (i) cold-start delta — a
  short-lived CLI workload + a first-N-requests server-shaped workload (reuse/adapt
  `bench/profiling/server_request.as` if LANE's corpus has landed; else commit a local
  variant), seeded vs unseeded archive, interleaved; (ii) steady-state — the bench corpus
  seeded vs unseeded, gate ≈1.0× (a regression is a bug); (iii) section-absent runs vs
  pre-WARM baseline ≈1.0× (zero-cost-when-off for the loader scan). Append the Unit-B table +
  the honest §3.7 framing to `bench/WARM_RESULTS.md`.
- [ ] **Step 4: Run everything** — differential both configs; property/fuzz builds; bench.
- [ ] **Step 5: Commit** — `test+bench(pgo): seeded differential mode + adversarial-seed fuzz axis + honest A/B (WARM B §5/§6, Gates 15-18)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer sabotage-tests the differential (force the seeder to
  install a TRUSTED index once — bypassing derivation — and confirm the corpus/fuzz axis
  catches it, then revert); re-runs the bench and checks the report's numbers and framing
  match the measurements (no over-claim).

## Task 9 (C): Durability option surface — enum + hardening

**Files:** `src/stdlib/workflow.rs`, `tests/workflow_durability.rs` (new)

- [ ] **Step 1: Write the failing tests:** `read_options` accepts `"fsync"` (and absent ⇒
  fsync), `"buffered"`, `"group"` (+ `groupWindowMs`/`groupMaxEvents` overrides, defaults
  50/128, validation: non-positive/non-finite ⇒ Tier-2 error); an UNKNOWN string
  (`"groop"`, `"full"`, `"async"`) ⇒ Tier-2 error naming the valid set (the §4.2 hardening —
  note `"full"`/`"async"` are deliberately NOT aliases; the error message teaches the real
  names). Both engines: run the same option-error program via `run_source` and
  `vm_run_source` — identical messages.
- [ ] **Step 2: Run — expect FAIL** (unknown strings silently mean fsync today —
  `workflow.rs:388`).
- [ ] **Step 3: Implement:**

```rust
/// WARM C (§4.2): the parsed durability policy. Default Fsync = today's behavior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Durability {
    Fsync,                                     // snapshot-at-finish + F_FULLFSYNC (unchanged)
    Group { window_ms: f64, max_events: usize }, // per-event append + coalesced fsync
    Buffered,                                  // snapshot-at-finish, no fsync (unchanged)
}
```

  `read_options` returns `(String, Durability)`; the existing `fsync: bool` threading through
  `workflow_run`/`finish_workflow` becomes `Durability` (Fsync/Buffered behavior verbatim —
  the existing `write_log` call sites map `Fsync ⇒ fsync=true`, `Buffered ⇒ false`; Group is
  Task 10).
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs (workflow is
  feature-gated: `--no-default-features` skips the module — confirm green).
- [ ] **Step 5: Commit** — `feat(workflow): Durability enum + unknown-value hardening (WARM C §4.2)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer confirms `"fsync"`/`"buffered"`/absent behavior is
  bit-for-bit today's (the existing `write_log_tests` + workflow suites untouched and green)
  and the error message lists exactly the three valid values.

## Task 10 (C): The group appender — chokepoint, pump, crc framing, repair-on-open

**Files:** `src/det.rs`, `src/stdlib/workflow.rs`, `src/stdlib/mod.rs`,
`tests/workflow_durability.rs`

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn group_appends_each_event_as_it_is_recorded() {
    // Run a 3-activity workflow under "group" where activity k asserts (via std::fs
    // from inside the activity) that the log already contains k-1 ActivityCompleted
    // lines — proving write-at-record-time (the kill-9 guarantee's mechanism).
}

#[test]
fn group_records_carry_crc_and_resume_replays_them() {
    // After a group run: every appended line parses, carries "crc", crc verifies;
    // resume() on the completed log returns the recorded result (idempotent path).
}

#[test]
fn torn_tail_is_repaired_by_prefix_truncation() {
    // PROPERTY BATTERY: take a valid group log from a 5-activity run; for EVERY byte
    // offset t in (0..len): copy, truncate to t, resume() → completes with the correct
    // final result; the repaired file's prefix is valid; activities lost to truncation
    // re-executed (side-effect markers), replayed ones did not double.
}

#[test]
fn seq_discontinuity_stops_the_prefix() {
    // Hand-edit a mid-file line's seq: repair truncates from there (contiguous-prefix
    // rule, §4.4) — the suffix re-executes on resume.
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** (spec §4.3/§4.4):
  - **Chokepoint refactor:** `DeterminismContext::record_event(&mut self, ev: DetEvent)` =
    `self.events.push(ev)` + `self.pump()`; convert the 11 `det.rs` push sites, the 3
    `workflow.rs` sites, and `stdlib/mod.rs:782`. Pure refactor for Fsync/Buffered (no
    appender installed ⇒ `pump()` is a `None` no-op — the SP9 inert-when-off pattern).
  - **Appender:** `GroupAppender { file: std::fs::File, persisted: usize, unsynced: usize,
    oldest_unsynced: Option<std::time::Instant>, window_ms: f64, max_events: usize }` held as
    `Option<GroupAppender>` ON the `DeterminismContext` (it already owns `events`; `!Send` is
    fine). `pump()`: serialize `events[persisted..]` (each record = today's `event_to_json`
    + `"crc"` field = CRC32 over the record bytes sans crc), ONE `write_all` for the batch,
    update counters, then `maybe_fsync()` (`unsynced >= max_events ||
    oldest_unsynced.elapsed() >= window`  ⇒ `sync_all` + reset). All sync code inside the
    recording call — no borrow across await by construction.
  - **Open/repair:** `open_group_log(path) -> (File, Vec<DetEvent>)`: read; find the valid
    prefix (newline-terminated + JSON-valid + crc-if-present + seq-contiguous); `set_len` to
    the prefix end; parse the prefix via the EXISTING `log_to_events`; seed
    `persisted = parsed_count`; position at end (append mode).
  - **Finish under Group:** append the `WorkflowCompleted` record (with crc, no seq — match
    `completed_result`'s last-line check), final `maybe_fsync()` (deadline-checked, NOT
    forced — spec §4.3), drop the appender. `write_log` is NOT called on the group path.
  - Mode wiring in `workflow_run`: Group ⇒ install the appender (fresh run: create/truncate;
    resume: open/repair, then the replay events come from the repaired prefix — keep the
    `completed_result` idempotent short-circuit reading the repaired text).
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs; the vm_differential
  workflow examples stay green (crash-free byte-identity across modes — add a three-mode
  output-identity test over `examples/`' workflow program here if not already covered).
- [ ] **Step 5: Commit** — `feat(workflow): group appender — record_event chokepoint, crc framing, prefix repair (WARM C §4.3-4.4)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer audits: (1) every former `events.push` site routes
  through `record_event` (grep — zero direct pushes remain outside the method); (2) NO
  unconditional fsync at finish on the group path (the bench win depends on it; the contract
  table documents it); (3) `ENOSPC` injection (full tmpfs or a 0-length-quota dir where
  available) surfaces a clean Tier-2 error; (4) the repair never EXTENDS a file and handles a
  zero-byte/garbage-only log (⇒ fresh-run semantics).

## Task 11 (C): The `kill -9` crash-recovery battery

**Files:** `tests/workflow_durability.rs` (spawn section)

- [ ] **Step 1: Write the failing battery** (spawn the real binary — the `tests/cli.rs`
  precedent; each case gets a unique temp dir; the workflow program: 5 activities, each
  appending a marker line to `markers.txt`, and after activity 3 the program writes
  `ready.txt` then spins on `time.sleep` so the parent can kill it):

```rust
fn run_until_ready_then_kill9(dir: &Path, durability: &str) { /* spawn `ascript run wf.as
    -- <durability>`, poll for ready.txt (timeout 30s), libc::kill(pid, SIGKILL), wait */ }

#[test]
fn kill9_fsync_mode_loses_in_flight_run_and_reexecutes_all() {
    // After kill at activity 3: log absent/old. Resume (a second spawn running resume):
    // completes; markers.txt shows activities 1-3 TWICE, 4-5 once (at-least-once,
    // today's contract — now pinned).
}

#[test]
fn kill9_group_mode_loses_nothing_and_replays_the_prefix() {
    // After kill at activity 3: log holds events 1..3 (page cache survives process
    // death). Resume: markers show 1-3 ONCE (replayed, not re-executed), 4-5 once;
    // final result correct; log ends with WorkflowCompleted; a second resume is
    // idempotent (returns the result, markers unchanged).
}

#[test]
fn kill9_mid_activity_group_mode_reexecutes_only_that_activity() {
    // ready.txt written INSIDE activity 4 before its marker: kill lands mid-activity;
    // resume re-executes activity 4 exactly (its event was never recorded) — the
    // in-flight-activity at-least-once edge.
}
```

  Unix-gated (`#[cfg(unix)]`) — SIGKILL semantics; the rest of the suite stays portable.
- [ ] **Step 2: Run — expect** the group tests FAIL only if Task 10 has a real bug (they are
  end-to-end checks of shipped code); the fsync-mode test should PASS immediately (it pins
  Task 0's baseline end-to-end). Fix anything that surfaces — that is this task's purpose.
- [ ] **Step 3: Run — expect PASS** ×20 in a loop (`for i in $(seq 20)` — crash tests must
  not flake; fix nondeterminism, don't retry-mask it).
- [ ] **Step 4: Commit** — `test(workflow): kill -9 crash-recovery battery — fsync vs group loss windows (WARM C §5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer runs the battery 50×, inspects a killed-run log by
  hand (lines complete, crc present), and verifies the marker-count assertions encode EXACTLY
  the spec §4.5 table (any mismatch = spec or code bug, resolved here, never papered over).

## Task 12 (C): Per-mode bench + example + workflow docs

**Files:** `bench/profiling/workflow_long.as`, `bench/run_warm_bench.sh`,
`bench/WARM_RESULTS.md`, `examples/advanced/workflow_durability.as`,
`docs/content/stdlib/workflow.md`

- [ ] **Step 1:** `bench/profiling/workflow_long.as` — ONE workflow, 2 000 activities (the
  per-event shape); keep `workflow_loop` as the per-commit shape. Extend
  `bench/run_warm_bench.sh`: both workloads × {fsync, group, buffered}, interleaved,
  medians + RSS; **gate:** `"fsync"` numbers vs pre-WARM baseline ≈1.0× (the default pays
  nothing). Append the Unit-C table to `bench/WARM_RESULTS.md` with the loss-window contract
  reproduced next to the numbers (the reader must see what each column buys).
- [ ] **Step 2:** `examples/advanced/workflow_durability.as` — production-shaped: an
  order-processing workflow run under `"group"` with idempotent activities (the documented
  guidance), full error handling, comments stating the loss-window contract; verify with
  `target/release/ascript run` AND `--tree-walker` (identical output); fmt-idempotent.
- [ ] **Step 3:** `docs/content/stdlib/workflow.md` — a "Durability" section: the three-mode
  table from spec §4.2 verbatim-in-spirit (write granularity, fsync policy, kill-9, power
  loss), the at-least-once activity contract + idempotency guidance, `groupWindowMs`/
  `groupMaxEvents`, the crc/repair note, and the explicit "the default is full durability;
  group/buffered are opt-in per workflow" framing. (Existing page — **no NAV change**.)
- [ ] **Step 4: Run** the bench; record honest numbers (expectation: order-of-magnitude on the
  fsync-dominated shapes under group — but the table reports whatever is measured).
- [ ] **Step 5: Commit** — `bench+docs(workflow): per-mode A/B + durability example + docs (WARM C §6, Gates 13/16/18)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer re-runs the bench, serves the docs site
  (`cd docs && python3 -m http.server`) and checks the workflow page renders with the table,
  and runs the example on all engines.

## Task 13: Docs (CLI/runtime), status updates, holistic review, FINAL GATES, merge

**Files:** `docs/content/cli.md`, `docs/content/runtime.md`, `CLAUDE.md`, `goal-perf.md`,
`superpowers/roadmap.md`, `superpowers/specs/2026-06-12-warm-starts-design.md` (status header)

- [ ] **Step 1: Docs:**
  - `docs/content/cli.md`: `run` gains the compile-cache paragraph (`--no-cache`,
    `ASCRIPT_NO_COMPILE_CACHE`, what invalidates, the uncached paths list), the
    `ascript cache clean|dir` subcommand, `build --pgo` (what a training run is — a REAL run;
    composes with `--native`; `ASCRIPT_NO_PGO`).
  - `docs/content/runtime.md`: the compile-cache mechanism (key inputs, verify-on-hit,
    fail-open) + the PGO section (what is seeded, the guard-absorbed soundness sentence, the
    honest expectations sentence).
  - Confirm NO new NAV entry is needed (existing pages) and record that check (Gate 13).
- [ ] **Step 2: Status:**
  - `CLAUDE.md`: a WARM paragraph (compile cache under `$ASCRIPT_CACHE/compiled/` —
    fail-open, verify-on-hit, path-in-key rationale; PGO trailing section — no version bumps,
    seeds-behind-guards, kill switch; workflow durability modes — default unchanged,
    record_event chokepoint, group contract).
  - `goal-perf.md`: WARM 🏗️ → ✅ in the spec table with headline numbers.
  - `superpowers/roadmap.md`: the WARM milestone entry + the recorded follow-ups (cache
    auto-GC; PGO profile merging; method-IC seeding; group-mode background flusher) — each
    with an owner note, none silent.
  - Spec status header → `Implemented (merged <sha>)` with any deltas-from-spec recorded.
- [ ] **Step 3: FINAL GATES CHECKLIST** (every box requires pasted command output in the task
  log — evidence before assertions):
  - [ ] `cargo clippy --all-targets` clean AND `cargo clippy --no-default-features
        --all-targets` clean.
  - [ ] `cargo test` green AND `cargo test --no-default-features` green.
  - [ ] `cargo test --test vm_differential` green in BOTH configs (incl. the PGO seeded
        projection + corpus).
  - [ ] `cargo test --test compile_cache --test pgo --test workflow_durability` green; the
        crash battery looped ×20 green; fuzz targets compile (`cd fuzz && cargo build`); a
        ≥10-min `cargo fuzz run pgo_section` + the seeded differential axis session where
        available, no findings (or fixed in-branch with regression tests).
  - [ ] `cargo test --release --test vm_bench -- --ignored --nocapture`: spec/tw geomean ≥2×
        and the DBG zero-cost gate re-run green (Gate 17 floor — WARM touches the archive
        LOAD path; prove no load-path tax leaked into steady state).
  - [ ] `bench/WARM_RESULTS.md` complete (all three unit tables + RSS, same-session
        methodology stated); `"fsync"` workflow numbers ≈ baseline; steady-state PGO ≈1.0×.
  - [ ] `ASO_FORMAT_VERSION` still 27 and `ARCHIVE_VERSION` still 1:
        `git diff main -- src/vm/aso.rs | grep -c ASO_FORMAT_VERSION` shows no constant
        change; Task 0's tolerance pin green.
  - [ ] Tree-walker untouched: `git diff main -- src/interp.rs` contains no behavioral change
        (justify any test/doc-only diff line-by-line).
  - [ ] Examples run on all modes; fmt-idempotent; docs site serves.
  - [ ] No new `unwrap`/`expect`/`panic!` reachable from untrusted input (cache bytes, PGO
        section, log files) — reviewer grep + justification list.
- [ ] **Step 4: Holistic review** — a fresh reviewer subagent reviews the WHOLE branch diff
  against the spec: the §2.9/§4.5 failure-mode tables vs actual code paths, the §3.5
  soundness argument vs the three install sites, the §4.3 empty-barrier decision vs the
  at-least-once tests, cross-unit interactions (a `--pgo` artifact in the compile cache? —
  must be impossible: the cache never stores PGO sections, verify; a cached run of a workflow
  program under each durability mode), and latent bugs in neighbors (`compile_archive` after
  the Task-1 extraction, `read_options` callers, the worker archive-install path now also
  seeding). All findings fixed in-branch with regression tests before merge.
- [ ] **Step 5: Merge** — `git checkout main && git merge --no-ff feat/warm-starts` with a
  summary merge message (house trailer). Update `goal-perf.md` status table post-merge.

---

## Standing rules for every task (repeated so no subagent misses them)

1. **Bug-fix discipline:** any defect encountered — WARM code or pre-existing, surfaced
   directly or incidentally — gets a failing-test-first fix **in this branch**, logged in the
   task notes with its commit. A known bug left in the tree is a campaign-blocking defect.
2. **Never relax an assertion.** Cached==uncached, seeded==unseeded, and the per-mode
   crash-free identity are Gate-1 contracts; a divergence is a bug in the unit, full stop.
3. **Both feature configs, every time.** A task is not green until `--no-default-features`
   is (Units A/B are core/CLI — they MUST build there; Unit C is `workflow`-feature-gated —
   the chokepoint refactor in `det.rs` is core and must compile without it).
4. **No borrow across await; no env reads in parallel tests** (thread explicit flags through
   `#[doc(hidden)]` seams — the LANE Task-2 convention).
5. **Untrusted bytes are hostile:** the cache manifest, the PGO section, and the workflow log
   are all attacker-writable files; every reader bounds-checks, never panics, and fails to
   the safe side (miss / warm-normally / prefix-truncate).
6. **Honest numbers only:** every performance claim in docs/reports traces to a committed,
   same-session measurement in `bench/WARM_RESULTS.md`. Expectations are stated; results are
   measured; a shortfall is reported, not massaged.
