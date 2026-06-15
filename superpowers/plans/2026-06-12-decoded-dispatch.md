# Decoded Dispatch (DECODE) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. A final **holistic
> review** covers the whole branch before merge. A task is closed only when every box under it
> is ticked.

**Goal:** Build the per-`FnProto`, lazily-decoded, fixed-width instruction stream (Unit A) the
LANE sync driver executes from; fuse the empirically-hottest record sequences into decoded-only
superinstructions (Unit B); speculatively inline small hot global fns behind the
`struct_gen`+identity guard with single-branch fallback (Unit C, droppable by evidence); cache
the burst's top-of-stack in a Rust local behind the complete §7.2 flush-edge contract (Unit D,
ships LAST, gated on Unit B's residual stack-traffic data, droppable by evidence). `Chunk.code`
stays byte-identical; every debugger/coverage byte-patch invalidates
affected decoded streams via the `Chunk.patch_epoch` chokepoint — the invalidation contract the
future JIT consumes.

**Architecture:** Spec: `superpowers/specs/2026-06-12-decoded-dispatch-design.md` (read it FULLY
before any task — the §2 record layout, §3 mapping discipline, §4 invalidation contract,
§5/§6/§7 unit designs (§7.2 is the TOS FLUSH INVARIANT — load-bearing), and §10 decided
narrowings are binding). New module `src/vm/decode.rs`
(`DecodedChunk`/`DecodedInstr`/`DOp`/decoder/peephole/inline transform); `Chunk` gains
`patch_epoch`/`decoded`/`decode_warmth` side fields; `run_loop_sync`'s burst loop body is
extracted as `sync_burst` (DECODE's own name — LANE exports `run_loop_sync`/`SyncOutcome`) and
made generic over an `InstrSource` (ByteSource = LANE's shipped behavior, RecordSource = the decoded
stream; Unit D adds a TOS-aware accessor layer inside RecordSource only); the async `run_loop`
is UNTOUCHED. Canonical ip is the byte ip; record indices never escape a burst; the TOS cache
never survives a burst exit.

**Tech stack:** Rust (single binary `ascript`); the bytecode VM (`src/vm/run.rs`,
`src/vm/chunk.rs`, `src/vm/opcode.rs`, post-LANE `run_loop_sync`); tests via `cargo test` (BOTH
feature configs), `tests/vm_differential.rs`, `tests/vm_bench.rs`, `tests/property.rs`,
`fuzz/fuzz_targets/differential.rs`, `bench/ab.sh` + `bench/profiling/` (LANE Task 0).

**HARD PRECONDITION:** **LANE is merged to `main`.** This plan consumes `run_loop_sync` /
`SyncOutcome` / `Vm.sync_lane` / the lane counters / `bench/ab.sh` /
`vm_run_source_no_sync_lane`. (`sync_burst` is DECODE's own extraction, Task 4 — if LANE's
implementation already factored an inner burst helper, Task 0 records its name and Task 4
reuses it.) Do not start otherwise. Every `file:line` in the spec was verified
PRE-LANE; Task 0 re-greps them.

**Binding execution standards (non-negotiable):**
- TDD per task: failing test → minimal code → green → commit. Frequent commits, house trailer on
  every commit: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Production-grade mandate (goal.md Gates 1–14):** any bug found while working — ours or
  pre-existing, direct or incidental — is fixed **in this branch** with a failing-test-first
  regression guard, never stepped around. No placeholders, no silent deferrals.
- Byte-identity is never relaxed: decoded-on == decoded-off == tree-walker, output AND panic
  message AND span. Fix the decoder/driver/guard, never the assertion.
- Clippy clean AND tests green under `--all-targets` and `--no-default-features --all-targets`
  before any "done" claim. Evidence (command output) before assertions.
- `Chunk.code`, `.aso`, `verify.rs`, `disasm.rs`, `src/interp.rs` byte-untouched (`git diff
  main --` empty for each; doc-only diffs justified line-by-line).
- Branch: `feat/decoded-dispatch` off `main`. Merge `--no-ff` after holistic review.

---

## File Structure

**New files:**
- `src/vm/decode.rs` — `DecodedInstr`, `DOp`, `DecodedChunk`, `InlineSegment`, the decoder,
  the fusion peephole, `FUSION_CANDIDATES`, the inline-candidate predicate + transform.
- `tests/decode_census.rs` — the `#[ignore]`d dynamic pair/triple census harness
  (`decode-census` feature).
- `tests/vm_decode.rs` — the DECODE-focused integration battery (invalidation, guards,
  coverage assertions; the corpus modes stay in `vm_differential.rs`).
- `bench/DECODE_PAIR_CENSUS.md` — committed census data (Task 7).
- `bench/DECODE_RESULTS.md` — the same-session A/B + RSS + threshold + residual-stack-traffic
  metric + inline-verdict + tos-verdict report (Task 11).
- `examples/advanced/decode_hot_loop.as` — hot-loop + small-global-fn example (corpus-joining,
  four-mode byte-identical; the breakpoint-during-hot-loop edge is exercised by
  `tests/vm_decode.rs` since examples can't drive the DAP).

**Modified files:**
- `src/vm/chunk.rs` — `Chunk.patch_epoch` (bumped inside `Chunk::patch_byte`), `decoded`,
  `decode_warmth`.
- `src/vm/run.rs` — `Vm.decode`/`Vm.decode_inline`/`Vm.decode_tos` + stat counters;
  `sync_burst` genericized over `InstrSource`; `RecordSource` driver arms (fused/inline
  extension match) + the Unit-D TOS accessor layer; frame-entry source selection + validity
  check.
- `src/vm/mod.rs` — `pub(crate) mod decode;`.
- `src/lib.rs` — `ASCRIPT_NO_DECODE`/`ASCRIPT_NO_DECODE_INLINE`/`ASCRIPT_NO_DECODE_TOS` seams;
  test entries `vm_run_source_no_decode` / `vm_run_source_decoded_forced` /
  `vm_run_source_decoded_no_inline` / `vm_run_source_decoded_no_tos` /
  `vm_run_source_decode_stats`.
- `Cargo.toml` — the default-off `decode-census` feature.
- `tests/vm_differential.rs` — decoded modes in the batteries + corpus + coverage assertions.
- `tests/vm_bench.rs` — decoded on/off section; Gate-12/17 + `dbg_zero_cost_gate` re-runs.
- `tests/property.rs`, `fuzz/fuzz_targets/differential.rs` — the decoded projections.
- `bench/PROFILING_RESULTS.md` — the post-DECODE re-profile section (the JIT gate input).
- `CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md`, the spec status header — Task 12.

---

## Task 0: Branch + grounding re-verification (post-LANE)

**Files:** none committed (notes only; fixes — if any divergence is found — are their own commits).

- [x] **Step 1:** `git checkout main && git pull && git checkout -b feat/decoded-dispatch`.
  Confirm LANE is merged: `grep -n "fn run_loop_sync\|sync_lane" src/vm/run.rs`
  must hit. If not — **STOP**; this plan is blocked on LANE.
- [x] **Step 2:** Re-grep every spec §11 citation that Tasks below rely on and record the
  post-LANE line numbers in the task log: `Op::operand_width`, `Code::patch_byte`,
  `Chunk::patch_byte`, the six patch sites (`arm_coverage`, the two trap-arm un-patches,
  `apply_set_breakpoints`, `apply_clear_breakpoints`, `dap/launch.rs` entry patch),
  `enter_frame_depth`/`leave_frame_depth`, `return_from_frame`, `eval_binop_adaptive`,
  `struct_gen`/`define_user_global`/`update_user_global`, `run_loop_sync`'s burst arm list vs
  LANE spec §3 (record the name of any inner burst helper LANE factored — Task 4's `sync_burst`
  extraction reuses it).
- [x] **Step 3:** Baseline runs (evidence for later A/Bs): full suite green both configs;
  `cargo test --release --test vm_bench -- --ignored --nocapture` recorded (pre-DECODE
  geomeans).
- [x] **Reviewer checkpoint:** reviewer confirms LANE presence, spot-checks 5 re-grepped
  anchors, and that the baseline bench output is filed in the task log.

## Task 1: The invalidation chokepoint — `Chunk.patch_epoch` (+ inert decode slots)

**Files:**
- Modify: `src/vm/chunk.rs`
- Test: inline `#[test]`s in `src/vm/chunk.rs` + a tripwire test in `tests/vm_decode.rs` (new file)

- [x] **Step 1: Write the failing tests** (in `chunk.rs` `mod tests`):

```rust
#[test]
fn patch_byte_bumps_the_patch_epoch_on_set_and_restore() {
    let mut c = Chunk::new();
    c.emit(Op::Nil, s(0, 1));
    c.emit(Op::Add, s(2, 3));
    assert_eq!(c.patch_epoch.get(), 0, "fresh chunk starts at epoch 0");
    // Set (the DBG breakpoint write) bumps…
    c.patch_byte(1, Op::Break as u8);
    assert_eq!(c.patch_epoch.get(), 1);
    // …and RESTORE bumps too (an un-patch also stales any stream that baked the Break).
    c.patch_byte(1, Op::Add as u8);
    assert_eq!(c.patch_epoch.get(), 2);
}

#[test]
fn decode_side_slots_default_empty() {
    let c = Chunk::new();
    assert!(c.decoded.borrow().is_none());
    assert_eq!(c.decode_warmth.get(), 0);
}
```

  And in `tests/vm_decode.rs`, the **chokepoint tripwire** (spec §8.4 #5 — keeps every patch
  site routed through the epoch-bumping `Chunk::patch_byte`):

```rust
/// DECODE §4.1: `Code::patch_byte` (the raw UnsafeCell write) must be reachable
/// ONLY through `Chunk::patch_byte` (which bumps `patch_epoch`). A future patch
/// site calling the raw Code method would silently skip invalidation — this
/// source scan trips on it. (The behavioral proof is the Task-6 battery; this
/// is the cheap structural guard.)
#[test]
fn raw_code_patch_byte_has_no_callers_outside_chunk_rs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    visit(&root, &mut |path, text| {
        if path.ends_with("vm/chunk.rs") { return; } // the definition + the one sanctioned caller
        for (i, line) in text.lines().enumerate() {
            // `chunk.patch_byte(`/-style calls are fine (they bump); the raw form is
            // `code.patch_byte(` / `.code.patch_byte(` — flag those.
            if line.contains("code.patch_byte(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    });
    assert!(offenders.is_empty(), "raw Code::patch_byte callers bypass patch_epoch:\n{}",
        offenders.join("\n"));
}
```

  (Implementer: write the small `visit` walker in the test file; verify the existing six patch
  sites — spec §4.1 table — all already call `Chunk::patch_byte` or `chunk.patch_byte`, i.e.
  the scan is green once the epoch lands. `set_breakpoint_shared` routes through
  `chunk.patch_byte` already — verified at `instrument.rs:268-280`.)

- [x] **Step 2: Run — expect FAIL** (`patch_epoch` doesn't exist):
  `cargo test --lib chunk::tests && cargo test --test vm_decode`
- [x] **Step 3: Implement** in `chunk.rs`:
  - Fields on `Chunk` (beside `overflow`): `pub patch_epoch: std::cell::Cell<u64>`,
    `pub decoded: RefCell<Option<Rc<crate::vm::decode::DecodedChunk>>>` (this task: declare the
    module with an empty placeholder struct? **NO placeholders** — declare the fields in THIS
    task only for `patch_epoch`; add `decoded`/`decode_warmth` in Task 3 where `DecodedChunk`
    exists. Adjust the Step-1 `decode_side_slots_default_empty` test to land in Task 3 instead.)
  - `Chunk::patch_byte` gains the bump:
    `self.patch_epoch.set(self.patch_epoch.get().saturating_add(1));` BEFORE delegating to
    `self.code.patch_byte(off, b)` (doc-comment: the DECODE §4.1 chokepoint — set AND restore
    both bump; any decoded/compiled artifact recording an older epoch is stale).
- [x] **Step 4: Run — expect PASS**; full `cargo test` + clippy, both configs (nothing else may
  move — the epoch is write-only until Task 4).
- [x] **Step 5: Commit** — `feat(vm/decode): Chunk.patch_epoch — the byte-patch invalidation chokepoint (DECODE §4.1)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer greps all `patch_byte` callers and confirms each routes
  through the bumping method; confirms `arm_coverage` on an N-line proto bumps the epoch N
  times (add a quick unit if not covered); confirms zero behavior change (full differential).

## Task 2: Kill switches + stat counters + test entry points (inert)

**Files:**
- Modify: `src/vm/run.rs` (Vm fields/constructors), `src/lib.rs`
- Test: `tests/vm_decode.rs`

- [x] **Step 1: Write the failing test:**

```rust
#[tokio::test]
async fn decode_entry_points_exist_and_are_inert_pre_driver() {
    let src = "let s = 0\nfor (i in 0..100) { s = s + i }\nprint(s)";
    let on = ascript::vm_run_source(src).await.expect("default ok");
    let off = ascript::vm_run_source_no_decode(src).await.expect("no-decode ok");
    let forced = ascript::vm_run_source_decoded_forced(src).await.expect("forced ok");
    assert_eq!(on, off);
    assert_eq!(on, forced);
    // Pre-driver, every counter reads 0 in every mode.
    let st = ascript::vm_run_source_decode_stats(src).await.expect("stats ok");
    assert_eq!((st.decoded_ops, st.fused_ops, st.inline_hits, st.inline_misses,
                st.decoded_bytes, st.stack_ops, st.tos_ops), (0, 0, 0, 0, 0, 0, 0));
}
```

- [x] **Step 2: Run — expect FAIL** (entries don't exist).
- [x] **Step 3: Implement:**
  - `Vm` fields beside `sync_lane`: `decode: bool`, `decode_inline: bool`, `decode_tos: bool`,
    `decode_threshold: u16` (the test knob; production default = `DECODE_THRESHOLD`, a
    `pub(crate) const` in `decode.rs` — placeholder value 8, **pinned by Task-11 data**),
    counters `decoded_ops/fused_ops/inline_hits/inline_misses/decoded_bytes/stack_ops/tos_ops:
    Cell<u64>` + accessors (`stack_ops` = fiber-stack pushes+pops retired by the record driver
    — the Unit-D gate input, spec §7.3; `tos_ops` = records retired with the TOS cache active).
    Constructor defaults from env (`ASCRIPT_NO_DECODE`, `ASCRIPT_NO_DECODE_INLINE`,
    `ASCRIPT_NO_DECODE_TOS` — mirroring `ASCRIPT_NO_SYNC_LANE`; env read at construction so
    worker isolates inherit; explicit constructor for tests never reads env — parallel-test
    hygiene, the LANE Task-2 pattern).
  - `src/lib.rs`: thread the four knobs through `vm_run_source_cfg`; add the five
    `#[doc(hidden)]` entries (stats returns a small `pub struct DecodeStats` — avoids a 9-tuple).
    Mention all three env vars beside `ASCRIPT_NO_SPECIALIZE` in the CLI `run` path comment.
- [x] **Step 4: Run — expect PASS**; clippy + full suite both configs.
- [x] **Step 5: Commit** — `feat(vm/decode): decode/decode_inline/decode_tos kill switches + stat counters + test entries (inert)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer greps that NO dispatch line changed; env handling
  matches the `ASCRIPT_NO_SPECIALIZE`/`ASCRIPT_NO_SYNC_LANE` precedents; tests never set env.

## Task 3: `DecodedChunk` + the 1:1 decoder (no fusion, no inlining, not yet executed)

**Files:**
- Create: `src/vm/decode.rs`; modify `src/vm/mod.rs`, `src/vm/chunk.rs` (the two side slots)
- Test: inline `#[test]`s in `src/vm/decode.rs`

- [x] **Step 1: Write the failing tests** (decoder correctness against the byte stream itself —
  the disassembler-walk idiom from `bcanalysis`):

```rust
#[test]
fn decode_one_to_one_covers_every_instruction_and_widens_operands() {
    // Build a chunk exercising every operand SHAPE (spec §2.1): zero-op, u8,
    // u16, i16 jump, u16+u8, u16+i16.
    let mut c = Chunk::new();
    let k = c.add_const(Value::Float(1.0));
    c.emit_u16(Op::Const, k, s(0, 1));            // u16
    c.emit_u8(Op::Call, 2, s(1, 2));              // u8
    c.emit_u16_u8(Op::DefineGlobal, 3, 1, s(2, 3)); // u16+u8
    let site = c.emit_jump(Op::Jump, s(3, 4));    // i16 (forward)
    c.emit(Op::Add, s(4, 5));                     // zero-op
    c.patch_jump(site);                            // target = end
    c.emit(Op::Return, s(5, 6));
    let d = decode_chunk(&c, &DecodeCfg::plain()).expect("decodes");
    // One record per instruction, in order, off = the opcode byte's offset.
    let offs: Vec<u32> = d.records.iter().map(|r| r.off).collect();
    let mut walk = Vec::new(); // independent operand_width walk
    let mut ip = 0;
    while ip < c.code.len() {
        walk.push(ip as u32);
        ip += 1 + Op::from_u8(c.code[ip]).unwrap().operand_width();
    }
    assert_eq!(offs, walk);
    // Operands widened: Const's u16 in `a`; DefineGlobal's u16 in `a`, u8 in `b`.
    assert!(matches!(d.records[0].op, DOp::Base(Op::Const)));
    assert_eq!(d.records[0].a, k as u32);
    assert_eq!((d.records[2].a, d.records[2].b), (3, 1));
}

#[test]
fn decode_resolves_jump_targets_to_record_indices() {
    // while-loop shape: JumpIfFalse forward over the body, Loop back to the test.
    let mut c = Chunk::new();
    c.emit(Op::True, s(0, 1));                       // 0: rec 0 (loop head)
    let exit = c.emit_jump(Op::JumpIfFalse, s(1, 2)); // 1: rec 1
    c.emit(Op::Nil, s(2, 3));                        // 4: rec 2
    c.emit(Op::Pop, s(3, 4));                        // 5: rec 3
    c.emit_loop(Op::Loop, 0, s(4, 5));               // 6: rec 4 → target rec 0
    c.patch_jump(exit);                               // → target rec 5
    c.emit(Op::Return, s(5, 6));                     // 9: rec 5
    let d = decode_chunk(&c, &DecodeCfg::plain()).unwrap();
    assert_eq!(d.records[1].a, 5, "JumpIfFalse pre-resolved to the Return record");
    assert_eq!(d.records[4].a, 0, "Loop pre-resolved to the head record");
    // entry_index is sorted by byte off and binary-searchable.
    for (i, r) in d.records.iter().enumerate() {
        assert_eq!(byte_to_record(&d, r.off), Some(i as u32));
    }
}

#[test]
fn decode_reads_current_bytes_a_patched_break_decodes_as_break_record() {
    let mut c = Chunk::new();
    c.emit(Op::Nil, s(0, 1));
    c.emit(Op::Add, s(1, 2));
    c.emit(Op::Return, s(2, 3));
    c.patch_byte(1, Op::Break as u8); // a live breakpoint
    let d = decode_chunk(&c, &DecodeCfg::plain()).unwrap();
    assert!(matches!(d.records[1].op, DOp::Base(Op::Break)),
        "a patched byte bakes a Break (escalation) record — §4.3 soundness");
    assert_eq!(d.own_epoch, c.patch_epoch.get(), "decoded AT the post-patch epoch");
}

#[test]
fn decoder_refuses_anomalous_jump_targets() {
    // A hand-corrupted mid-instruction jump target (unreachable from the
    // compiler; the .aso verifier rejects it at the trust boundary) must yield
    // None — permanent byte-dispatch fallback, never a bad record.
    let mut c = Chunk::new();
    c.emit_u16(Op::Const, 0, s(0, 1));
    let site = c.emit_jump(Op::Jump, s(1, 2));
    c.emit(Op::Return, s(2, 3));
    c.code[site..site + 2].copy_from_slice(&(-4i16).to_le_bytes()); // lands mid-Const
    assert!(decode_chunk(&c, &DecodeCfg::plain()).is_none());
}
```

- [x] **Step 2: Run — expect FAIL** (module doesn't exist).
- [x] **Step 3: Implement `src/vm/decode.rs`** per spec §2.2:

```rust
pub(crate) struct DecodedInstr { pub op: DOp, pub a: u32, pub b: u32, pub off: u32 }

pub(crate) enum DOp {
    Base(Op),
    // Unit B variants land in Task 8; Unit C's in Task 9.
}

pub(crate) struct DecodedChunk {
    pub records: Vec<DecodedInstr>,
    pub entry_index: Vec<(u32, u32)>, // sorted (byte_off, record_idx), caller-chunk records
    pub own_epoch: u64,
    pub deps: Vec<(Rc<FnProto>, u64)>,        // Task 9 fills; empty until then
    pub inline_segments: Vec<InlineSegment>,  // Task 9; empty until then
}

pub(crate) struct DecodeCfg { /* fuse: bool, inline: Option<&Vm-side resolver>, threshold… */ }

/// Decode `chunk` 1:1 into fixed-width records with jump targets pre-resolved
/// to record indices. Returns None on any structural anomaly (the caller falls
/// back to byte dispatch for this proto, permanently).
pub(crate) fn decode_chunk(chunk: &Chunk, cfg: &DecodeCfg) -> Option<Rc<DecodedChunk>> {
    // Pass 1: walk code via Op::from_u8 + operand_width (the bcanalysis idiom,
    // src/vm/bcanalysis.rs:151-160); collect (off, op, a, b) reading operands
    // by shape (read_u16/read_u8/read_i16 — spec §2.1 table); build off→idx.
    // Pass 2: rewrite jump operands: target_byte = off + 1 + width + disp;
    //         a = index_of[target_byte]? else return None.
    //         Also collect the ENTRY-POINT set (record 0, every jump target,
    //         record-after-escalation, record-after-Yield) for Task 8's peephole.
    // Pass 3 (Task 8): peephole fusion. Pass 4 (Task 9): inlining.
    // own_epoch = chunk.patch_epoch.get() read AFTER pass 1 (single-threaded:
    // no patch can interleave, but read-late is the conservative order).
}

pub(crate) fn byte_to_record(d: &DecodedChunk, off: u32) -> Option<u32> { /* binary search */ }
```

  Add to `Chunk`: `pub decoded: RefCell<Option<Rc<DecodedChunk>>>`,
  `pub decode_warmth: Cell<u16>` (+ the Task-1 deferred `decode_side_slots_default_empty`
  test). `DecodedChunk` needs no `Debug` detail — give `Chunk`'s derive a manual `Debug` skip or
  a stub `impl Debug` (implementer: `Chunk` derives `Debug`; simplest is
  `#[derive(Debug)]`-compatible via `impl fmt::Debug for DecodedChunk` printing record count).
  The escalation-op classifier reuses LANE's `sync_lane_op` (export it `pub(crate)` from
  `run.rs` — single source of truth; do NOT write a second allowlist).
- [x] **Step 4: Run — expect PASS**; clippy + full suite both configs (decoder is dead code so
  far — allow `#[allow(dead_code)]` ONLY if clippy demands, removed in Task 4).
- [x] **Step 5: Commit** — `feat(vm/decode): DecodedChunk + 1:1 decoder, jump targets pre-resolved (DECODE §2)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer cross-checks the decode walk against `disasm_at`'s walk
  on a real compiled program (compile `fib` via the test compile path, decode, assert offs ==
  disasm offsets); probes `JumpIfArgSupplied` (u16+i16 — the jump operand is the SECOND word)
  and `DefineGlobal` (u16+u8) widening; confirms `Break` records when bytes are pre-patched.

## Task 4: Execute from records — `InstrSource` genericization + the RecordSource driver

This is the riskiest refactor; it lands in two strictly-separated steps: (4a) a
behavior-preserving genericization with ONLY the byte source, full gates green; (4b) the record
source.

**Files:**
- Modify: `src/vm/run.rs`, `src/vm/decode.rs`
- Test: `tests/vm_decode.rs`, `tests/vm_differential.rs`

- [x] **Step 1 (4a): Extract `run_loop_sync`'s burst loop body as `sync_burst`, generic over
  `InstrSource`** (spec §2.4 — `sync_burst` is DECODE's name; reuse LANE's inner helper if Task
  0 found one): extract the
  fetch/advance/jump mechanics behind the trait; `ByteSource` reproduces LANE's exact behavior
  (decode `code[ip]`, subset check BEFORE advancing, escalation leaves ip un-advanced). The arm
  BODIES move verbatim — zero semantic edits. **Gate for 4a alone:** full `cargo test` both
  configs + `cargo test --test vm_differential` both configs + a `vm_bench` spot-run showing
  no regression vs Task 0's baseline. Commit separately:
  `refactor(vm/lane): sync_burst generic over InstrSource — byte source only, behavior-preserving` (house trailer).
- [x] **Step 2: Write the failing tests (4b):**

```rust
#[tokio::test]
async fn forced_decode_executes_records_and_counts_them() {
    // Anti-false-green (spec §8.3a): records must actually retire.
    let src = "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)";
    let st = ascript::vm_run_source_decode_stats(src).await.expect("ok"); // forced threshold=0
    assert_eq!(st.output, "499999500000\n");
    assert!(st.decoded_ops >= 1_000_000, "only {} records retired", st.decoded_ops);
    assert!(st.decoded_bytes > 0, "memory accounting must report");
}

#[tokio::test]
async fn no_decode_kill_switch_means_zero_records() {
    let st = ascript::vm_run_source_decode_stats_no_decode(
        "let s = 0\nfor (i in 0..1000) { s = s + i }\nprint(s)").await.expect("ok");
    assert_eq!(st.decoded_ops, 0);
}

#[tokio::test]
async fn decoded_on_off_byte_identical_incl_panics_and_spans() {
    for src in [
        "print(1 + 2 * 3)",
        "fn fib(n) { if (n < 2) { return n } return fib(n - 1) + fib(n - 2) }\nprint(fib(15))",
        "let o = { x: 0, y: 1 }\nfor (i in 0..1000) { o.x = o.x + o.y }\nprint(o.x)",
        "print(1 << 64)",                          // Tier-2 panic — message identical
        "fn f(n) { return f(n + 1) }\nprint(f(0))", // recursion-depth panic (SP3 §B)
        "let m = match 3 { 1..5 => \"in\", _ => \"out\" }\nprint(m)",
    ] {
        let tw = ascript::run_source(src).await;
        let on = ascript::vm_run_source_decoded_forced(src).await;
        let off = ascript::vm_run_source_no_decode(src).await;
        match (tw, on, off) {
            (Ok(t), Ok(a), Ok(b)) => { assert_eq!(t, a.0, "tw vs decoded `{src}`");
                                       assert_eq!(a, b, "decoded vs byte `{src}`"); }
            (Err(t), Err(a), Err(b)) => { assert_eq!(t.to_string(), a.to_string());
                                          assert_eq!(a.to_string(), b.to_string()); }
            other => panic!("ok/err disagreement on `{src}`: {other:?}"),
        }
    }
}

#[tokio::test]
async fn escalation_resumes_byte_identically_across_representations() {
    // A decoded burst escalating (await/method/import) must hand the async
    // driver an EXACT byte ip; the post-escalation burst re-enters via
    // entry_index. Async + method + generator shapes:
    for src in [
        "async fn a(x) { return x + 1 }\nlet f = a(41)\nprint(await f)",
        "class C { fn init() { self.n = 0 } fn bump() { self.n = self.n + 1 } }\nlet c = C()\nfor (i in 0..100) { c.bump() }\nprint(c.n)",
        "fn* g(n) { for (i in 0..n) { yield i * i } }\nlet t = 0\nfor await (v in g(5)) { t = t + v }\nprint(t)",
    ] {
        let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
        let off = ascript::vm_run_source_no_decode(src).await.expect("ok");
        assert_eq!(on, off, "diverged on `{src}`");
    }
}

#[tokio::test]
async fn cross_module_panic_provenance_survives_the_hoisted_source_refresh() {
    // Spec §2.4: last_fault_source is refreshed per FRAME under decoded
    // dispatch (per-chunk constant). A panic in an imported module must render
    // with the same provenance decoded vs not. (Implementer: reuse the SP4 §3
    // multi-module fixture idiom from the existing cross-module tests —
    // tempdir + a module file whose fn panics; compare full rendered errors.)
}
```

- [x] **Step 3: Run — expect FAIL** (no record source).
- [x] **Step 4: Implement (4b):**
  - `RecordSource<'a>` over `Rc<DecodedChunk>` + burst-local `idx: u32`: `fetch` reads
    `records[idx]` (subset check on `DOp::Base(op)` via the shared `sync_lane_op`; out-of-subset
    or pending-await ⇒ `sync_ip` writes `records[idx].off` and escalates); `jump` is
    `idx = target`; `sync_ip` writes `records[idx].off` (on `Err`, the FAULTING record's off —
    sync before propagating).
  - Frame-entry source selection in `run_loop_sync`: if `!self.decode` → byte. Else read
    `chunk.decoded`: `Some(d)` and **valid** (`d.own_epoch == chunk.patch_epoch.get()` && deps
    && the §6.6 instrument rule — deps/instrument trivially pass until Task 9) → records;
    invalid → `decoded.take()` + byte + re-warm. `None` → bump `decode_warmth`; at
    `self.decode_threshold` run `decode_chunk` (install or permanently-byte on `None`).
  - Counter flushes: burst-local accumulators flushed at every exit incl. `Err` (LANE §6.4
    costing; the flush sits OUTSIDE the `?`). `stack_ops` (fiber-stack pushes+pops retired by
    the record driver) counts in the same burst-local way — it is Unit D's §7.3 gate input,
    so it exists from the first record executed.
  - Stats entries wired (`vm_run_source_decode_stats*` set threshold 0 + return the counters).
- [x] **Step 5: Run — expect PASS**; then the FULL differential + property suite + clippy, both
  configs.
- [x] **Step 6: Commit** — `feat(vm/decode): RecordSource — the sync driver executes pre-decoded records (DECODE §2.4-§2.5, §3)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer (a) audits every `sync_ip` call site — escalation,
  finish, `Err`, frame push — and constructs a test where a burst escalates at a record whose
  PREVIOUS record was a jump (the likeliest off-by-one); (b) confirms `fiber.frame().ip` after
  a decoded escalation equals the byte-dispatch value (instrument both modes on the same
  program); (c) greps zero `.await` in the decode driver; (d) re-runs the corpus differential
  BOTH configs; (e) confirms 4a's commit alone was green (checks the CI/log evidence).

## Task 5: Differential modes + fuzz axis + corpus coverage assertions (Gate 15)

**Files:**
- Modify: `tests/vm_differential.rs`, `tests/property.rs`, `fuzz/fuzz_targets/differential.rs`

- [x] **Step 1:** Extend the standing identity everywhere it runs (expression batteries,
  program batteries, goldens, whole-corpus gate — extend the existing helper fns, never
  duplicate loops): add `vm_run_source_decoded_forced` and `vm_run_source_no_decode`
  projections (and `vm_run_source_decoded_no_inline` once Task 9 lands, plus
  `vm_run_source_decoded_no_tos` once Task 10 lands — leave a marked extension point). Both
  feature configs.
- [x] **Step 2:** The corpus coverage assertion (spec §8.3, the LANE §6.4 idiom):

```rust
#[tokio::test]
async fn decoded_dispatch_actually_executes_on_the_corpus() {
    let mut total_records: u64 = 0; let mut total_bytes: u64 = 0; let mut ran = 0usize;
    for rel in all_corpus_examples() { // the SAME enumeration + skip list as oracle #1
        if skip_reason(&rel).is_some() { continue; }
        let src = std::fs::read_to_string(corpus_path(&rel)).unwrap();
        if feature_unavailable_in_this_build(&src).await { continue; }
        if let Ok(st) = ascript::vm_run_source_decode_stats(&src).await {
            total_records += st.decoded_ops; total_bytes += st.decoded_bytes; ran += 1;
        }
    }
    println!("DECODE corpus coverage: {total_records} records / {total_bytes} decoded bytes over {ran} programs");
    assert!(ran > 30, "corpus enumeration broke");
    assert!(total_records > 1_000_000,
        "decoded dispatch retired only {total_records} records — silently collapsed");
}
```

- [x] **Step 3:** Fuzz axis, same PR: `fuzz/fuzz_targets/differential.rs` adds the
  `vm_run_source_decoded_forced` + `vm_run_source_no_decode` projections to the per-input
  equality + crash report; mirror in `tests/property.rs` (the generated-program battery + the
  fixed-seed battery). `cd fuzz && cargo build` must compile.
- [x] **Step 4: Sabotage-test the tripwires** (reviewer repeats independently): temporarily
  make the frame-entry selection always choose byte dispatch → the coverage assertion FAILS;
  revert. Temporarily corrupt a jump-resolution off-by-one → the property battery or corpus
  FAILS; revert.
- [x] **Step 5: Run** — `cargo test --test vm_differential` + `--test property` (BOTH configs),
  ≥10-min `cargo fuzz run differential` where available.
- [x] **Step 6: Commit** — `test(decode): decoded differential modes + fuzz axis + corpus coverage assertion (Gate 15)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer re-runs both sabotages, bumps the property case count,
  runs the fuzzer, and confirms the corpus assertion's printed share appears in CI logs.

## Task 6: The invalidation battery (the JIT-contract proof)

**Files:**
- Modify: `tests/vm_decode.rs` (battery items 1 & 3 & 4), `src/vm/run.rs` (only if a bug falls out)
- The DAP-driven item reuses the in-crate hook harness idiom from `run.rs`'s own
  `parked_clear_breakpoints_restores_the_original_byte` test (`run.rs:7206`) — drive
  `DebuggerHook` + command channel directly, no editor needed.

- [x] **Step 1: Write the failing tests** (spec §8.4 — items 2 and 5 land with Task 9/Task 1
  respectively; here: 1, 3, 4):

```rust
#[tokio::test]
async fn breakpoint_set_mid_hot_loop_invalidates_the_decoded_stream_and_fires() {
    // §8.4 #1 (THE mandatory test): warm a loop until its proto decodes, then
    // patch a breakpoint at the loop line through the hook machinery, run
    // again — the trap MUST fire (a stale stream would sail past it), then
    // clear + resume → byte-identical final output, and decoding resumes.
    //
    // Implementer: build via the in-crate harness — compile the program, build
    // the Vm with forced threshold, run once (assert decode_stats show records
    // + chunk.decoded.borrow().is_some()), then DebuggerHook::new() +
    // set_breakpoint_shared(proto_id, loop_offset, &chunk) [epoch bumps], spawn
    // the controller thread replying Continue, run again, assert: (a) a
    // Stopped event arrived; (b) chunk.decoded was dropped at the next frame
    // entry; (c) output across the whole sequence == an uninstrumented run;
    // (d) after the un-patch + re-warm, decoded_ops rises again.
}

#[tokio::test]
async fn coverage_over_decoded_execution_is_byte_identical_and_complete() {
    // §8.4 #3: --coverage of a hot-loop program. arm_coverage patches every
    // line start (epoch bumps) BEFORE the run, so first executions trap on
    // byte dispatch / Break records, un-patch (epoch bumps), and the hot loop
    // then decodes + runs from records. Assert: program output == no-decode
    // run; the covered line set == the no-decode run's covered set.
}

#[test]
fn decoded_chunk_validity_unit_tests() {
    // §8.4 #4: built at epoch N → invalid after one patch_byte (set), invalid
    // again after restore; a deps entry with a stale epoch invalidates even
    // when own_epoch matches (cross-proto — the Unit-C hole, testable now with
    // a hand-built deps vec).
}
```

- [x] **Step 2: Run — expect FAIL or PASS-for-the-wrong-reason** — verify each test FAILS when
  the validity check is stubbed to `true` (sabotage first, then implement/confirm the real
  check makes it pass; this battery must be demonstrably capable of catching the bug it
  guards).
- [x] **Step 3:** Fix anything the battery surfaces (failing-test-first; in-branch).
- [x] **Step 4: Run — expect PASS**; full suite both configs.
- [x] **Step 5: Commit** — `test(decode): the byte-patch invalidation battery — breakpoints/coverage vs decoded streams (DECODE §4, §8.4)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer probes the edges: set a breakpoint on the SAME offset
  twice (idempotent set — one epoch bump per patch call, side table keeps the true original);
  clear-all while parked (`apply_clear_breakpoints` restores N bytes = N bumps); break-on-entry
  (`dap/launch.rs` offset 0) on a program whose entry proto is already decoded by a previous
  REPL-style run on the same Vm.

## Task 7: Unit B part 1 — the census (feature, harness, committed data)

**Files:**
- Modify: `Cargo.toml` (feature `decode-census = []`), `src/vm/decode.rs` + `src/vm/run.rs`
  (cfg-gated counting), `src/lib.rs` (cfg-gated census entry)
- Create: `tests/decode_census.rs`, `bench/DECODE_PAIR_CENSUS.md`

- [x] **Step 1:** Implement the counting mode, **fully `#[cfg(feature = "decode-census")]`**
  (compiled out of every default build — the JIT-spec §2.1 not-there discipline; zero Gate-12
  exposure): the record driver, when the feature is on AND a Vm census flag is set, records
  consecutive `(DOp, DOp)` pairs and triples (burst-local `prev`/`prev2`, flushed into a
  `RefCell<HashMap<(u16,u16,u16), u64>>` keyed by discriminants) **within basic blocks only**
  (reset `prev` at jumps/escalations/entry points — never count across a boundary fusion
  could not legally cross).
- [x] **Step 2:** `tests/decode_census.rs` — `#[ignore]`d, mirrors `vm_bench`'s big-stack
  thread + current-thread-runtime idiom (`vm_bench.rs:374-392`): runs every
  `bench/profiling/*.as` + the runnable corpus (the `all_corpus_examples` enumeration) in
  forced-decode census mode, aggregates, prints the ranked pair/triple table with dynamic
  counts and % of total records.
- [x] **Step 3:** Run it:
  `cargo test --release --features decode-census --test decode_census -- --ignored --nocapture`
  and commit the output verbatim (machine, date, command, table) as
  `bench/DECODE_PAIR_CENSUS.md`.
- [x] **Step 4:** Verify the default build is census-free: `cargo build` then
  `grep` the census symbols out of `nm`/no — simpler: `cargo clippy --all-targets` (no
  feature) compiles the counting code OUT (confirm via `#[cfg]` review + the zero-cost bench
  re-run in Task 11).
- [x] **Step 5: Commit** — `feat(decode): decode-census feature + harness; commit pair/triple census data (DECODE §5.1)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer re-runs the census (numbers may differ slightly —
  RANKS must be stable), confirms the basic-block reset (no pair counted across a jump
  target — probe with a crafted two-block program), and that the default build contains no
  census code path.

## Task 8: Unit B part 2 — `FUSION_CANDIDATES` + the peephole + fused arms

**Files:**
- Modify: `src/vm/decode.rs` (peephole + `DOp` variants), `src/vm/run.rs` (fused arms in the
  record driver's extension match), `tests/vm_decode.rs`, `tests/vm_differential.rs`
  (re-run modes)

- [x] **Step 1:** From the committed census, select **≤ 8** fused forms that (a) rank top by
  dynamic count, (b) fit the record payload (all base operands ≤ u16 — pack two per u32, spec
  §2.1; reserve one u32 word for the fault offset wherever a non-first component can panic),
  (c) compose ONLY shared-helper calls (spec §5.3 — a candidate needing reimplemented
  semantics is recorded-and-rejected in the constant's doc-comment). Ship as:

```rust
/// DECODE §5: the REVIEWED fusion set. Each entry cites its census line
/// (bench/DECODE_PAIR_CENSUS.md, run of <date>) and documents its a/b packing
/// + fault-offset word. Changing this set requires a refreshed census commit.
pub(crate) const FUSION_CANDIDATES: &[FusedForm] = &[
    // e.g. (ILLUSTRATIVE — the census decides):
    // GetLocal s1; GetLocal s2; <BinOp>  → LLBinOp { a: s1|s2<<16, b: binop_off }
    //   (op kind folded into the DOp variant or a packed nibble — implementer
    //    picks the encoding that keeps the driver arm branch-free)
    // GetLocal s; Const k; <BinOp>       → LConstBinOp { … }
    // GetLocal s; GetProp name_k         → LGetProp { a: s|k<<16, b: prop_off }
];
```

- [x] **Step 2: Write the failing tests:**

```rust
#[tokio::test]
async fn fused_records_execute_and_are_counted() {
    // The numeric-loop shape must produce fused records (whatever the census
    // chose, a local+local/local+const arithmetic loop is in every realistic set).
    let st = ascript::vm_run_source_decode_stats(
        "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)").await.expect("ok");
    assert_eq!(st.output, "499999500000\n");
    assert!(st.fused_ops > 0, "no fused records retired — the peephole is dead");
}

#[tokio::test]
async fn fused_panic_attributes_to_the_faulting_component() {
    // An overflow inside a fused arithmetic record: message AND rendered span
    // must equal the unfused (no-decode) run and the tree-walker. Whole
    // rendered error compared, not just the message (the span is the contract).
    let src = "let x = 9223372036854775807\nlet y = 1\nfor (i in 0..20) { y = x + y }\nprint(y)";
    let tw  = ascript::run_source(src).await.expect_err("panics");
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect_err("panics");
    let off = ascript::vm_run_source_no_decode(src).await.expect_err("panics");
    assert_eq!(on.to_string(), off.to_string());
    assert_eq!(on.to_string(), tw.to_string());
}

#[test]
fn peephole_never_fuses_across_an_entry_point() {
    // §5.2: a jump target / after-Yield / after-escalation record is never a
    // fused MIDDLE. Build a loop whose back-edge targets the would-be middle
    // (decode a crafted chunk; assert the records around the target stay 1:1
    // and entry_index still contains the target's off).
}

#[tokio::test]
async fn adaptive_cache_keys_are_unchanged_under_fusion() {
    // §5.3: a fused arith component passes the SAME fault_ip to
    // eval_binop_adaptive, so the arith_caches map keys on the same offset in
    // both representations. Run hot decoded, then inspect
    // chunk.arith_caches keys == the no-decode run's keys.
}
```

- [x] **Step 3: Run — expect FAIL**; implement the peephole (a single left-to-right pass over
  the 1:1 records using the Task-3 entry-point set; greedy longest-match against
  `FUSION_CANDIDATES`; fused record's `off` = first component's, fault word = the panicking
  component's) and the driver arms (each a straight composition of the existing shared
  helpers — `fiber.local`, `eval_binop_adaptive(fiber, fault_off, …)`, `ic_get_field`, …).
- [x] **Step 4: Run — expect PASS**; the FULL differential + property suites both configs (the
  five-way modes now exercise fusion corpus-wide); the Task-5 sabotage for `fused_ops`
  (disable the peephole → assertion fails → revert).
- [x] **Step 5: Record the post-fusion RESIDUAL stack-traffic share (Unit D's gate input,
  spec §7.3):** run the dispatch-bound trio (`object_churn`, `call_heavy`, `func_pipeline`)
  through `vm_run_source_decode_stats` and record `stack_ops / decoded_ops` per workload in
  the task log + a dated stub section of `bench/DECODE_RESULTS.md` (Task 11 folds it into
  the final report). For context, also record the same ratio with `FUSION_CANDIDATES`
  temporarily emptied (a local one-off run, not a shipped switch) — the delta shows how much
  traffic fusion already removed. This number decides whether Unit D (Task 10) is even
  attempted at full depth or fast-tracked to a RECORD-REJECT verdict.
- [x] **Step 6: Commit** — `feat(vm/decode): data-driven superinstructions — reviewed FUSION_CANDIDATES + decode-time peephole (DECODE §5)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer verifies every shipped candidate against the committed
  census (no candidate without a data line); diffs each fused arm against the sequence of
  unfused arms it replaces (helper-call-for-helper-call); probes: breakpoint set ON a fused
  middle's byte offset (epoch → re-decode → that run unfused → trap fires, output identical);
  a deopt (type-polymorphic operands) inside a fused arm matches unfused behavior.

## Task 9: Unit C — speculative global-fn inlining (behind its own toggle)

**Files:**
- Modify: `src/vm/decode.rs` (predicate + transform + `InlineSegment`), `src/vm/run.rs`
  (InlineEnter/InlineExit arms + the deps/instrument validity legs + `decode_inline` plumb to
  `DecodeCfg`), `tests/vm_decode.rs`, `tests/vm_differential.rs` (the no-inline mode joins the
  batteries)

- [x] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn small_global_fn_is_inlined_and_guard_hits_are_counted() {
    let src = r#"
fn add(a, b) { return a + b }
let s = 0
for (i in 0..1000000) { s = add(s, i) }
print(s)
"#;
    let st = ascript::vm_run_source_decode_stats(src).await.expect("ok");
    assert_eq!(st.output, "499999500000\n");
    assert!(st.inline_hits > 100_000, "inline guard never hit ({})", st.inline_hits);
    // And the no-inline toggle kills exactly this:
    let st2 = ascript::vm_run_source_decode_stats_no_inline(src).await.expect("ok");
    assert_eq!(st2.output, st.output);
    assert_eq!(st2.inline_hits, 0);
    assert!(st2.decoded_ops > 0, "no-inline must not kill decoding");
}

#[tokio::test]
async fn guard_miss_struct_gen_and_identity_both_fall_back_byte_identically() {
    // (i) struct_gen miss: a NEW top-level define after the warm loop bumps the
    //     gen (NOTE — spec §8.3: REdefining is a runtime error, so the miss is
    //     forced by defining a NEW global, then calling again).
    let gen_miss = r#"
fn add(a, b) { return a + b }
let s = 0
for (i in 0..100000) { s = add(s, i) }
let unrelated = 1
for (i in 0..100000) { s = add(s, i) }
print(s + unrelated)
"#;
    // (ii) identity miss: a mutable `let` global rebound to a different closure
    //      (update_user_global does NOT bump struct_gen — run.rs; the identity
    //      leg of the guard is what catches this).
    let id_miss = r#"
let f = (x) => x + 1
let s = 0
for (i in 0..100000) { s = f(s) }
f = (x) => x + 2
for (i in 0..100000) { s = f(s) }
print(s)
"#;
    for src in [gen_miss, id_miss] {
        let tw = ascript::run_source(src).await.expect("tw ok");
        let st = ascript::vm_run_source_decode_stats(src).await.expect("decoded ok");
        let off = ascript::vm_run_source_no_decode(src).await.expect("byte ok");
        assert_eq!(tw, st.output_exit(), "tw vs decoded on `{src}`");
        assert_eq!(st.output_exit(), off, "decoded vs byte on `{src}`");
        assert!(st.inline_misses > 0, "the miss path never ran on `{src}`");
    }
}

#[tokio::test]
async fn inlined_call_counts_one_logical_call_depth_unit() {
    // SP3 §B byte-identity: deep recursion THROUGH an inline-eligible leaf at
    // the boundary. `leaf` is inline-eligible at its call site inside `deep`;
    // `deep` itself is recursive (not inlineable). The depth panic must be
    // byte-identical in all three modes — proving InlineEnter bumps the
    // counter exactly once per logical call.
    let src = r#"
fn leaf(a) { return a + 1 }
fn deep(n) { return deep(leaf(n)) }
print(deep(0))
"#;
    let tw  = ascript::run_source(src).await.expect_err("panics").to_string();
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect_err("panics").to_string();
    let off = ascript::vm_run_source_no_decode(src).await.expect_err("panics").to_string();
    assert_eq!(on, off); assert_eq!(on, tw);
    assert!(on.contains("maximum recursion depth exceeded"));
}

#[tokio::test]
async fn panic_inside_an_inlined_body_keeps_the_callee_span_and_source() {
    // §6.4: the faulting record's off is a CALLEE-chunk offset; the rendered
    // error (message + caret position) equals the no-decode run. Use an
    // arithmetic type panic inside the leaf body.
    let src = "fn bad(a) { return a + \"x\" }\nlet s = 0\nfor (i in 0..100000) { s = bad(s) }\nprint(s)";
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect_err("panics");
    let off = ascript::vm_run_source_no_decode(src).await.expect_err("panics");
    assert_eq!(on.to_string(), off.to_string());
}

#[tokio::test]
async fn breakpoint_inside_an_inlined_callee_invalidates_the_caller_stream() {
    // §8.4 #2 — the deps-epoch proof: warm a caller that inlined `f`; set a
    // breakpoint on a line INSIDE f (patches f's chunk → f.patch_epoch bumps;
    // the CALLER's own_epoch is untouched); call again → the caller's stream
    // must be dropped via deps and the trap must fire in f's REAL frame.
    // Built on the Task-6 hook harness.
}

#[tokio::test]
async fn profiler_armed_disables_inline_segments_only() {
    // §6.6: with a deterministic profiler armed, callee frames must appear in
    // the sample set exactly as in a no-decode run (inline segments invalid);
    // decoded_ops still > 0 (plain records keep running).
}
```

- [x] **Step 2: Run — expect FAIL**; implement per spec §6:
  - **Predicate** (in `decode.rs`, near the `bcanalysis` static-check style): the §6.1 clause
    list, verbatim, over the candidate site's `GET_GLOBAL f; …; CALL argc` byte shape and the
    resolved proto (resolution via a `DecodeCfg` callback the Vm supplies —
    `decode.rs` stays Vm-free/pure-analysis; the Vm closure reads `get_user_global_full`).
  - **Transform:** splice the callee's 1:1-decoded, slot-rewritten body between `InlineEnter`
    (a/b = packed `(global_idx, recorded_struct_gen)` + the fallback record index; the
    recorded proto `Rc` + callee-chunk epoch go into `deps` + the `InlineSegment` row) and
    `InlineExit`; record `inline_segments` ranges for span/source attribution.
  - **Driver arms:** `InlineEnter` — guard (`struct_gen` == recorded && slot at recorded idx is
    a Closure with `Rc::ptr_eq` proto); miss → `inline_misses` bump + jump to the fallback
    record (the untouched `Base(Call)`); hit → `inline_hits` bump, `enter_frame_depth(span)?`,
    `stack.remove(callee_idx)`, pad `Nil` to `slot_count`, set burst-local `inline_base`,
    swap `last_fault_source` to the callee's. `InlineExit` — pop value,
    `truncate(inline_base)`, `leave_frame_depth()`, push, restore `last_fault_source`.
  - **Validity legs:** frame-entry check adds deps-epoch comparison and the
    `inline_segments.non_empty() ⇒ instrument.borrow().is_none()` rule.
  - The differential/no-inline mode joins Task 5's batteries at the marked extension point;
    the fuzz/property projections re-run.
- [x] **Step 3: Run — expect PASS**; FULL suite + differential + property, both configs;
  sabotage the guard (skip the identity leg → the `id_miss` test must FAIL → revert).
- [x] **Step 4: Commit** — `feat(vm/decode): speculative global-fn inlining — struct_gen+identity guard, single-branch fallback (DECODE §6)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer hunts the seams: a call site whose argc ≠ arity (must
  decode 1:1 and panic generically); a candidate with a typed param / default / rest / ret
  contract / cell slot (each must refuse — write a table-driven predicate test); nested
  inline-eligible calls (depth-1 rule holds); `stack.remove` vs the real arm's pop/push net
  effect on a site with argc 0 (callee_idx == TOS edge); the
  `breakpoint_inside_an_inlined_callee` test against a SECOND caller that also inlined `f`
  (both streams must drop).

## Task 10: Unit D — top-of-stack register caching (LAST; behind its own toggle)

**Files:**
- Modify: `src/vm/run.rs` (the `RecordSource` TOS accessor layer + flush edges + `decode_tos`
  plumb), `tests/vm_decode.rs` (the flush-edge battery), `tests/vm_differential.rs` (the
  no-tos mode joins the batteries at the Task-5 extension point), `tests/property.rs` +
  `fuzz/fuzz_targets/differential.rs` (the no-tos projection)

**Pre-step gate check (spec §7.3):** read Task 8's recorded residual stack-traffic share. If
fusion already drove `stack_ops/decoded_ops` to noise on the dispatch-bound trio, SKIP the
implementation steps, go straight to the Task-11 RECORD-REJECT verdict with that data, and
close this task with the verdict logged. Otherwise proceed.

- [x] **Step 1: Write the failing tests** — the coverage/kill-switch pair plus **one named test
  per §7.2 flush edge**, each engineered to cross its edge with a LIVE cached TOS:

```rust
#[tokio::test]
async fn tos_cached_bursts_execute_and_are_counted() {
    // Anti-false-green (spec §8.3e): TOS-cached records must actually retire.
    let st = ascript::vm_run_source_decode_stats(
        "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)").await.expect("ok");
    assert_eq!(st.output, "499999500000\n");
    assert!(st.tos_ops > 0, "no TOS-cached records retired — Unit D is dead");
    // The dedicated kill switch kills EXACTLY Unit D:
    let st2 = ascript::vm_run_source_decoded_no_tos(
        "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)").await.expect("ok");
    assert_eq!(st2.output, st.output);
    assert_eq!(st2.tos_ops, 0);
    assert!(st2.decoded_ops > 0, "no-tos must not kill decoding");
}

#[tokio::test]
async fn flush_edge_1_escalation_mid_expression() {
    // Edge 1: a pending await as a BINOP OPERAND — the burst escalates with the
    // lhs cached in TOS; the async driver must see it on fiber.stack.
    let src = r#"
async fn slow(x) { return x * 2 }
let total = 0
for (i in 0..1000) { total = total + await slow(i) }
print(total)
"#;
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw  = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off); assert_eq!(tw, on.0);
}

#[tokio::test]
async fn flush_edge_1_breakpoint_patched_mid_hot_loop_sees_the_flushed_stack() {
    // Edge 1 × the §8.4 battery: warm the loop (TOS hot), set a breakpoint at
    // the loop line via the hook harness, hit it, EVALUATE a local at the park
    // (the snapshot reads fiber.stack — a missed flush shows a wrong/missing
    // operand), clear, resume → byte-identical to an uninstrumented run.
    // (Builds on Task 6's hook-harness helper.)
}

#[tokio::test]
async fn flush_edge_3_panic_in_a_fused_record() {
    // Edge 3: an i64-overflow panic INSIDE a fused record while TOS is cached;
    // the rendered error AND any recover() inspection must match no-tos/tw.
    let src = r#"
fn run() {
  let x = 9223372036854775807
  let y = 1
  for (i in 0..20) { y = x + y }
  return y
}
let [v, e] = recover(() => run())
print(v, e)
"#;
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw  = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off); assert_eq!(tw, on.0);
}

#[tokio::test]
async fn flush_edge_4_call_at_cached_tos_state() {
    // Edge 4: a plain call whose LAST ARG is the cached TOS — check_call_args
    // and the callee window must see it physically on the stack. Also covers
    // InlineEnter (the inlined variant of the same callee).
    let src = r#"
fn add(a, b) { return a + b }
let s = 0
for (i in 0..100000) { s = add(s, i * 2 + 1) }
print(s)
"#;
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw  = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off); assert_eq!(tw, on.0);
}

#[tokio::test]
async fn flush_edge_2_yield_suspends_with_a_complete_stack() {
    // Edge 2: a generator yielding mid-expression-rich body; the suspended
    // fiber is re-entered by resume — its stack must be complete.
    let src = r#"
fn* squares(n) { for (i in 0..n) { yield i * i + (i + 1) } }
let total = 0
for await (v in squares(50)) { total = total + v }
print(total)
"#;
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    assert_eq!(on, off);
}

#[tokio::test]
async fn flush_edge_5_return_into_a_caller_mid_expression() {
    // Edge 5: the caller holds a cached partial (`1000 + …`) while the callee
    // returns through return_from_frame; the helper's truncate/push must
    // compose with the caller's flushed cache.
    let src = r#"
fn f(n) { return n * 3 }
let total = 0
for (i in 0..100000) { total = total + (1000 + f(i)) }
print(total)
"#;
    let on  = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw  = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off); assert_eq!(tw, on.0);
}
```

  (Implementer: validate program idioms against `examples/**` as usual; the per-edge ASSERTIONS
  are the contract, the programs may be reshaped — but each must demonstrably cross its edge
  with `tos` occupied, which the sabotage in Step 4 proves.)
- [x] **Step 2: Run — expect FAIL** (the counter assertions; the identity assertions should
  already pass — if one fails BEFORE the feature, that is a pre-existing bug: stop and fix it
  first per the production-grade mandate).
- [x] **Step 3: Implement** (spec §7.1–§7.2): a burst-local `TosCache { tos: Option<Value> }`
  accessor layer inside `RecordSource` ONLY (`push`/`pop`/`peek(n)` per §7.1; `decode_tos ==
  false` ⇒ the accessors pass straight through to `fiber.stack`). Flush
  (`if let Some(v) = tos.take() { fiber.stack.push(v) }` — idempotent) wired at EVERY §7.2
  edge: before `sync_ip` on `NeedsAsync`; on `Finished` (Return-root/Yield) after popping the
  result; on the `Err` path before propagating (same placement as the counter flush — OUTSIDE
  the `?`); before any Call-class record (`push_closure_frame` callees, native escalations) and
  `InlineEnter`; before `return_from_frame`/`InlineExit`. Reviewer-greppable invariant: every
  `return` out of the record burst and every shared-helper call that takes `&mut Fiber` for
  call/return mechanics is preceded by a flush or operates through the accessor layer.
- [x] **Step 4: Sabotage-verify the battery, edge by edge:** comment out ONE edge's flush at a
  time; the matching named test (and only correctness, not the others' timing) must FAIL;
  restore. Paste the per-edge evidence in the task log. Then full suite + differential +
  property, BOTH configs; the fuzz/property no-tos projection added at the Task-5 extension
  point.
- [x] **Step 5: Commit** — `feat(vm/decode): Unit D — TOS register cache in the record burst, complete flush-edge contract (DECODE §7)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer independently re-derives the flush-edge list from the
  driver's exit edges and diffs it against §7.2 (a NEW exit edge added since the spec means a
  NEW flush + test — check none was missed); probes `peek(1)`-class under-TOS arms
  (`MapEntry`/`AppendArray`/`Swap`/`Rot3`/`SetIndex`) with TOS occupied; re-runs the corpus
  differential both configs; greps that the async loop and `ByteSource` are untouched by
  Unit D.

## Task 11: Bench — Gate-12/17 re-runs, threshold A/B, same-session A/B, the unit verdicts

**Files:**
- Modify: `tests/vm_bench.rs`; Create: `bench/DECODE_RESULTS.md`; Modify:
  `bench/PROFILING_RESULTS.md`

- [x] **Step 1:** `vm_bench.rs` gains `Engine::NoDecodeVm` (→ `vm_run_source_no_decode`) and a
  `decode_on_off` section after the lane section: per-bench decoded(default)/no-decode
  medians + speedups + geomean. GATE: decoded-on shows **no regression** on any bench
  (≥ 0.97× noise bound); the speedup is REPORTED.
- [x] **Step 2:** Full harness run —
  `cargo test --release --test vm_bench -- --ignored --nocapture`. Required `[PASS]` rows:
  spec/tw geomean ≥ 2.0× (Gates 12/17), `dbg_zero_cost_gate` armed/none ≤ 1.05× (DECODE
  touches dispatch ⇒ mandatory re-run; per spec §6.6 the armed-idle config loses ONLY inline
  segments — record that delta explicitly in the header notes), cov/off re-reported, the new
  decode section. Append the dated GATE RESULT block to the file header (the standing
  convention).
- [x] **Step 3: Threshold A/B (spec §2.3):** with `bench/ab.sh` (same session): candidate
  built twice via env-free test knob? — no: run the SAME binary with
  `DECODE_THRESHOLD`-selecting env (add `ASCRIPT_DECODE_THRESHOLD` as a test-facing env read
  beside the kill switches) over the 8-workload corpus at threshold 0 vs 8 vs 32. Pin the
  shipped `DECODE_THRESHOLD` constant from the winner; record the table.
- [x] **Step 4: Same-session A/B (Gate 16):** baseline = merge-base worktree build, candidate
  = branch (`bench/ab.sh <base> <cand> 7`); plus the three isolating A/Bs on the candidate
  binary alone: `ASCRIPT_NO_DECODE=1` vs default (Units A+B contribution),
  `ASCRIPT_NO_DECODE_INLINE=1` vs default (Unit C contribution, over `call_heavy` +
  `func_pipeline` especially), and `ASCRIPT_NO_DECODE_TOS=1` vs default (Unit D contribution,
  over the dispatch-bound trio — run AFTER the Unit A+B numbers are recorded, per spec §7.3's
  ordering). Profile ≥ 1 workload with the shipped profiler
  (`--profile cpu` — Gate-16 dogfooding). Peak RSS per workload + total decoded bytes
  (Gate 18) — an RSS regression is a bug to fix before merge.
- [x] **Step 5: THE UNIT-C VERDICT (spec §6.7):** if the isolated inline win is **< 2% geomean
  on the call-heavy corpus**, Unit C is DROPPED: revert the Task-9 feature commits (keep the
  deps machinery + its tests — they are §4's), record the verdict + data in
  `bench/DECODE_RESULTS.md` and `goal-perf.md`. Either way the outcome is written down with
  numbers — never silent.
- [x] **Step 5b: THE UNIT-D VERDICT (spec §7.5) — both outcomes specified:**
  - **SHIP** iff the isolated tos-on win is **≥ 2% geomean on the dispatch-bound trio**
    (`object_churn` + `call_heavy` + `func_pipeline`) AND no workload anywhere regresses
    beyond the 0.97× noise bound. Record the table; `decode_tos` stays default-true with its
    permanent kill switch.
  - **RECORD-REJECT** otherwise (or if Task 10's pre-step gate already short-circuited on the
    residual-traffic data): revert the Task-10 feature commits (the accessor layer reverts to
    direct fiber ops; the flush-edge battery is deleted WITH the feature — it tests nothing
    without it; `stack_ops`/`tos_ops` counters stay, they are the recorded evidence), and
    write the verdict + the residual-stack-traffic share + the A/B table into
    `bench/DECODE_RESULTS.md` and `goal-perf.md`. Honest single digits were the stated
    expectation — a near-zero result is a legitimate, documented outcome, never silent.
- [x] **Step 6:** Write `bench/DECODE_RESULTS.md` (machine/date/methodology, the A/B tables,
  threshold table, the Task-8 residual-stack-traffic section, RSS + decoded-bytes table, the
  inline AND tos verdicts) and append the
  **post-DECODE re-profile** section to `bench/PROFILING_RESULTS.md` (bucket re-attribution
  on `object_churn`/`call_heavy` + the explicit **JIT gate verdict paragraph**: does dispatch
  still dominate? — `goal-perf.md`'s mandatory re-rank checkpoint).
- [x] **Step 7: Commit** — `bench(decode): Gate-12/17 re-runs + threshold A/B + same-session A/B + inline/tos verdicts + post-DECODE re-profile (Gates 16/18)` (house trailer).
- [x] **Reviewer checkpoint:** reviewer re-runs the harness independently; checks every number
  in the .md files against raw output; verifies baseline/candidate ran interleaved in one
  session; confirms the JIT-verdict paragraph follows from the data; if any bench regressed
  decoded-on beyond noise, that is a bug fixed here (likeliest: the frame-entry validity
  check's cost — fix its home, never the bound).

## Task 12: Example, docs, status, holistic review, gates checklist, merge

**Files:**
- Create: `examples/advanced/decode_hot_loop.as`
- Modify: `CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md`,
  `superpowers/specs/2026-06-12-decoded-dispatch-design.md` (status header)

- [x] **Step 1: The example** — `examples/advanced/decode_hot_loop.as`: a production-shaped,
  fully error-handled hot loop over small global fns + an arithmetic pipeline (the
  inline+fusion happy path) AND the edge: a mutable-`let` callback rebound mid-run (the
  guard-miss path runs in an ordinary program). Deterministic, run-to-completion,
  fmt-idempotent; joins the corpus (and therefore every differential mode + the coverage
  assertions). Verify on all four modes + `--tree-walker`. (The breakpoint-during-hot-loop
  edge lives in `tests/vm_decode.rs` — examples cannot drive the DAP; record that mapping
  here per Gate 9.)
- [x] **Step 2: Docs/status:**
  - `CLAUDE.md`: a DECODE paragraph in the VM architecture notes (the decoded side
    representation + the `patch_epoch` chokepoint + "drop, never edit" + the kill switches
    beside `--no-specialize`/`ASCRIPT_NO_SYNC_LANE` (incl. `ASCRIPT_NO_DECODE_TOS` if Unit D
    shipped, with the §7.2 flush invariant named) + "the census constant changes only with
    refreshed committed data" + the six-way differential identity).
  - `goal-perf.md`: DECODE 🏗️ → ✅ in the spec table; the post-DECODE re-profile pointer; the
    JIT-gate verdict; the Unit-C AND Unit-D verdicts (each shipped or dropped-by-evidence;
    the owner already removed the parked-list TOS line — Unit D is its disposition).
  - `superpowers/roadmap.md`: the DECODE milestone entry (what shipped, gates, headline
    numbers, the invalidation contract as the JIT prerequisite).
  - Spec status header → `Implemented (merged <sha>)` with any deltas-from-spec recorded (no
    silent deviation; the §10 narrowings are already recorded — only NEW deltas go here).
  - User-facing `docs/`: engine-internal, no surface change — confirm no page is stale and
    record that this was checked (Gate 13).
- [x] **Step 3: FINAL GATES CHECKLIST** (every box requires pasted command output in the task
  log — evidence before assertions):
  - [x] `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets`
        clean.
  - [x] `cargo test` AND `cargo test --no-default-features` green.
  - [x] `cargo test --test vm_differential` green BOTH configs (six-way identity — incl. the
        no-tos mode if Unit D shipped — + corpus + the decoded/fused/inline/tos coverage
        assertions).
  - [x] `cargo test --test property` green both configs; `cd fuzz && cargo build` compiles; a
        ≥10-min `cargo fuzz run differential` session where available, no findings (or
        findings fixed in-branch, failing-test-first).
  - [x] The invalidation battery AND (if Unit D shipped) the flush-edge battery green
        (`cargo test --test vm_decode`), including the sabotage evidence (each tripwire and
        each flush edge shown to FAIL when its path is disabled).
  - [x] `cargo test --release --test vm_bench -- --ignored --nocapture`: spec/tw ≥ 2×,
        `dbg_zero_cost_gate` ≤ 1.05×, decode section no-regression — all `[PASS]`; results
        recorded in the header block.
  - [x] `bench/DECODE_RESULTS.md` + `bench/DECODE_PAIR_CENSUS.md` + post-DECODE
        `PROFILING_RESULTS.md` committed; RSS + decoded-bytes + residual stack-traffic
        reported, no regression; the Unit-C and Unit-D verdicts recorded with numbers.
  - [x] Byte-identity of the untouched surfaces: `git diff main -- src/vm/aso.rs
        src/vm/verify.rs src/vm/disasm.rs src/interp.rs` empty (doc-only diffs justified
        line-by-line); `ASO_FORMAT_VERSION` unchanged; tree-sitter/fmt/LSP untouched (no
        surface change — recorded).
  - [x] No new `unwrap/expect/panic!` reachable from user input in touched code (reviewer
        grep + a justification list for VM-bug-invariant panics mirroring existing ones).
  - [x] `examples/**` emits 0 `type-*` diagnostics in BOTH configs (Gate 5 — the new example
        included).
- [x] **Step 4: Holistic review** — a fresh reviewer subagent reviews the WHOLE branch diff
  against the spec: record layout vs §2.2, the §3 ip-discipline at every driver exit, the §4
  epoch contract (incl. deps + the instrument rule), peephole boundary rule vs §5.2, fused
  arms as pure helper compositions, the §6.1 predicate clause-by-clause, guard completeness
  (struct_gen AND identity), the §7.2 flush-edge list re-derived from the final driver code
  (if Unit D shipped), SP3 depth accounting, and hunts latent bugs in neighbors
  (`entry_index` binary-search edges, `stack.remove` arithmetic, counter + TOS flush on `Err`,
  census basic-block resets). All findings fixed in-branch, failing-test-first, before merge.
- [x] **Step 5: Merge** — `git checkout main && git merge --no-ff feat/decoded-dispatch`
  (summary message, house trailer). Update `goal-perf.md` status table post-merge.

---

## Standing rules for every task (repeated so no subagent misses them)

1. **Bug-fix discipline:** any defect encountered — DECODE's or pre-existing, surfaced directly
   or incidentally — gets a failing-test-first fix **in this branch**, logged with its commit.
   A known bug left in the tree is a campaign-blocking defect.
2. **Never relax an assertion** to make modes agree. The tree-walker is the oracle; no-decode
   is the shipped byte path; decoded-forced must equal both, output + panic + span.
3. **Both feature configs, every time.** A task is not green until `--no-default-features` is.
4. **The decoded driver contains zero awaits** (it lives inside the sync lane); the reviewer
   greps `\.await` in `decode.rs` + the record-source paths and expects zero hits. No
   `RefCell` borrow across anything that can re-enter the VM.
5. **`Chunk.code` is read-only to DECODE.** Only the pre-existing `patch_byte` sites mutate
   bytes; DECODE adds the epoch beside them, never a new mutation.
6. **Spans and messages are part of the contract:** fused/inlined paths construct panics via
   the same shared helpers with the faulting component's offset; the differential's panic
   batteries and the corpus are the proof.
7. **No placeholder fusion candidates and no guessed thresholds:** the census data and the
   threshold A/B are committed BEFORE the constants they justify are pinned.
8. **The TOS cache never survives a burst exit (spec §7.2).** Any new exit edge added to the
   record driver — in this plan or any future one — ships WITH its flush and its named
   flush-edge test in the same commit. The Fiber externalizes complete state at every lane
   switch; that is the two-lane design's load-bearing invariant.
