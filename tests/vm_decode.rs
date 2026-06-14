//! DECODE structural guards (the decoded-dispatch effort).
//!
//! These are cheap source-scan tripwires that keep the invalidation chokepoint
//! intact; the behavioral proofs live in the differential + the Task-6 battery.

use std::path::Path;

/// Recursively walk every `.rs` file under `dir`, calling `f(path, contents)`.
fn visit(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, f);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                f(&path, &text);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE Task 2 — kill switches + stat counters + entry points (inert)
// ─────────────────────────────────────────────────────────────────────────────

/// DECODE Task 2/4: the three kill switches and the stats test entry point exist
/// and their outputs are byte-identical to the default VM. Pre-Task-4 every
/// counter was 0; Task 4 wires the RecordSource driver, so under the FORCED
/// (threshold-0) stats entry point `decoded_ops`/`decoded_bytes`/`stack_ops` now
/// rise above 0 while the FUSED/INLINE/TOS counters (Units B/C/D) stay 0.
#[tokio::test]
async fn decode_entry_points_exist_and_are_byte_identical() {
    let src = "let s = 0\nfor (i in 0..100) { s = s + i }\nprint(s)";
    let on = ascript::vm_run_source(src).await.expect("default ok");
    let off = ascript::vm_run_source_no_decode(src).await.expect("no-decode ok");
    let forced = ascript::vm_run_source_decoded_forced(src).await.expect("forced ok");
    assert_eq!(on, off, "decode-off must be byte-identical to default");
    assert_eq!(on, forced, "decode-forced must be byte-identical to default");
    // Task 4: the FORCED stats entry point (threshold 0) retires records, so the
    // wired counters are positive. Task 8 (Unit B): the loop body
    // (`s = s + i`) contains a `GetLocal; ...; Add`-style accumulate shape, so
    // fusion fires → `fused_ops > 0`. Task 10 (Unit D): every record retired with
    // the TOS cache active → `tos_ops > 0`. The Unit C inline counters stay 0
    // (no global fn to inline here).
    let st = ascript::vm_run_source_decode_stats(src).await.expect("stats ok");
    assert!(st.decoded_ops > 0, "RecordSource must retire records under forced decode");
    assert!(st.decoded_bytes > 0, "memory accounting must report");
    assert!(st.stack_ops > 0, "stack-traffic gate input must count from the first record");
    assert!(st.fused_ops > 0, "Unit B fusion must fire on the accumulate loop");
    assert!(st.tos_ops > 0, "Unit D TOS cache must fire under forced decode");
    assert_eq!(
        (st.inline_hits, st.inline_misses),
        (0, 0),
        "Unit C inline counters stay 0 (no inlinable global fn in this program)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE Task 4 — the RecordSource driver executes from records, byte-identically
// ─────────────────────────────────────────────────────────────────────────────

/// Anti-false-green (spec §8.3a): records must actually retire.
#[tokio::test]
async fn forced_decode_executes_records_and_counts_them() {
    let src = "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)";
    let st = ascript::vm_run_source_decode_stats(src).await.expect("ok"); // forced threshold=0
    assert_eq!(st.output, "499999500000\n");
    assert!(st.decoded_ops >= 1_000_000, "only {} records retired", st.decoded_ops);
    assert!(st.decoded_bytes > 0, "memory accounting must report");
}

#[tokio::test]
async fn no_decode_kill_switch_means_zero_records() {
    let st = ascript::vm_run_source_decode_stats_no_decode(
        "let s = 0\nfor (i in 0..1000) { s = s + i }\nprint(s)")
        .await
        .expect("ok");
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
            (Ok(t), Ok(a), Ok(b)) => {
                assert_eq!(t, a.0, "tw vs decoded `{src}`");
                assert_eq!(a, b, "decoded vs byte `{src}`");
            }
            (Err(t), Err(a), Err(b)) => {
                assert_eq!(t.to_string(), a.to_string());
                assert_eq!(a.to_string(), b.to_string());
            }
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

/// Reviewer checkpoint (a): a burst that escalates at a record whose PREVIOUS
/// record was a JUMP (the likeliest off-by-one — the record cursor must resync
/// from the canonical ip, not blindly `idx += 1`). The loop back-edge (`Op::Loop`)
/// is a jump; the very next instruction the loop body reaches can be an escalation
/// (a method call / await). We weave a loop whose body both jumps (the back-edge)
/// and escalates (a method call) so the resync-after-jump path is exercised every
/// iteration, then compare forced-decode to byte.
#[tokio::test]
async fn escalation_immediately_after_a_jump_is_byte_identical() {
    for src in [
        // while-loop back-edge then a (non-sync) method call escalation each iter.
        "class C { fn init() { self.n = 0 } async fn bump() { self.n = self.n + 1 } }\nlet c = C()\nlet i = 0\nwhile (i < 50) { await c.bump(); i = i + 1 }\nprint(c.n)",
        // for-range (lazy, jump-heavy) with an await of a resolved future in-body.
        "async fn id(x) { return x }\nlet t = 0\nfor (i in 0..50) { t = t + await id(i) }\nprint(t)",
        // nested loops: inner back-edge + outer back-edge, pure arithmetic.
        "let s = 0\nfor (i in 0..30) { for (j in 0..30) { s = s + i * j } }\nprint(s)",
    ] {
        let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
        let off = ascript::vm_run_source_no_decode(src).await.expect("ok");
        assert_eq!(on, off, "diverged on `{src}`");
    }
}

#[tokio::test]
async fn cross_module_panic_provenance_survives_the_hoisted_source_refresh() {
    // Spec §2.4: last_fault_source is refreshed per FRAME under decoded dispatch
    // (per-chunk constant). A panic in an imported module must render with the
    // same provenance decoded vs not. Multi-module fixture: a tempdir module file
    // whose fn panics, imported and called from the entry program.
    let dir = std::env::temp_dir().join(format!("ascript_decode_prov_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let modfile = dir.join("boom.as");
    std::fs::write(&modfile, "export fn boom(x) {\n  return x + nil\n}\n").expect("write mod");
    let entry = dir.join("main.as");
    std::fs::write(
        &entry,
        "import { boom } from \"./boom\"\nfn drive() { return boom(1) }\nprint(drive())\n",
    )
    .expect("write entry");

    // Run on the byte path and the forced-decode path; the rendered panic
    // (message + module provenance) must be identical.
    let off = ascript::run_file_no_decode(&entry).await;
    let on = ascript::run_file_decoded_forced(&entry).await;
    match (off, on) {
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "provenance diverged"),
        other => panic!("expected both to panic, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE §5 (Unit B / Task 8) — fused superinstructions: the peephole + arms
// ─────────────────────────────────────────────────────────────────────────────

/// §5: the numeric-loop shape must produce fused records (a local+local /
/// local+const arithmetic loop is in every realistic census set), and the result
/// must be correct + byte-identical.
#[tokio::test]
async fn fused_records_execute_and_are_counted() {
    let st = ascript::vm_run_source_decode_stats(
        "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)")
        .await
        .expect("ok");
    assert_eq!(st.output, "499999500000\n");
    assert!(st.fused_ops > 0, "no fused records retired — the peephole is dead");
}

/// §5.3: an overflow inside a fused arithmetic record — message AND rendered span
/// must equal the unfused (no-decode) run and the tree-walker. The WHOLE rendered
/// error is compared (the span is the contract, not just the message).
#[tokio::test]
async fn fused_panic_attributes_to_the_faulting_component() {
    let src = "let x = 9223372036854775807\nlet y = 1\nfor (i in 0..20) { y = x + y }\nprint(y)";
    let tw = ascript::run_source(src).await.expect_err("panics");
    let on = ascript::vm_run_source_decoded_forced(src).await.expect_err("panics");
    let off = ascript::vm_run_source_no_decode(src).await.expect_err("panics");
    assert_eq!(on.to_string(), off.to_string());
    assert_eq!(on.to_string(), tw.to_string());
}

/// §5.3: a fused GetProp/Add component passes the SAME fault offset to the field-IC
/// and the adaptive-arith path that the unfused arm would, so a field-read /
/// arithmetic loop is byte-identical (incl. the IC/adaptive warm path) decoded-and-
/// fused vs unfused. Drives the `GetLocal->GetProp->Add` triple + `GetProp->Add`
/// pair hard (the field-read-then-use spine, census triple rank 1).
#[tokio::test]
async fn fused_field_read_then_arith_is_byte_identical() {
    for src in [
        // self.field accumulate (GetLocal self; GetProp; Add) in a hot method loop.
        "class C { fn init() { self.n = 0 } fn step(x) { self.n = self.n + x } }\n\
         let c = C()\nfor (i in 0..5000) { c.step(i) }\nprint(c.n)",
        // object field arithmetic: o.x = o.x + o.y (GetLocal o; GetProp x; ... ).
        "let o = { x: 0, y: 3 }\nfor (i in 0..5000) { o.x = o.x + o.y }\nprint(o.x)",
        // local+const / const+local staging into a binop, plus local+local.
        "let a = 1\nlet b = 2\nlet s = 0\nfor (i in 0..5000) { s = a + b + s + i }\nprint(s)",
    ] {
        let tw = ascript::run_source(src).await.expect("tw ok");
        let on = ascript::vm_run_source_decoded_forced(src).await.expect("decoded ok");
        let off = ascript::vm_run_source_no_decode(src).await.expect("byte ok");
        assert_eq!(tw, on.0, "tw vs decoded+fused `{src}`");
        assert_eq!(on, off, "decoded+fused vs byte `{src}`");
    }
}

/// DECODE §7.3 (Unit D gate input): record the post-fusion RESIDUAL stack-traffic
/// share (`stack_ops / decoded_ops`) for the dispatch-bound trio. Run with
/// `--ignored --nocapture`; the printed ratios are folded into
/// `bench/DECODE_RESULTS.md`. To see the fusion delta, empty `FUSION_CANDIDATES`
/// and re-run (a local one-off, not a shipped switch).
#[tokio::test]
#[ignore]
async fn decode_residual_stack_traffic_share() {
    let root = env!("CARGO_MANIFEST_DIR");
    for name in ["object_churn", "call_heavy", "func_pipeline"] {
        let path = std::path::Path::new(root).join("bench/profiling").join(format!("{name}.as"));
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        let st = ascript::vm_run_source_decode_stats(&src).await.expect("ok");
        let ratio = st.stack_ops as f64 / st.decoded_ops.max(1) as f64;
        println!(
            "DECODE §7.3 residual: {name:<14} decoded_ops={:>10} fused_ops={:>10} stack_ops={:>10} stack/decoded={ratio:.3}",
            st.decoded_ops, st.fused_ops, st.stack_ops
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE §8.4 — the invalidation battery (the JIT-contract proof)
//
// The BEHAVIORAL halves of the battery (a breakpoint set after warmup must fire
// through an invalidated+rebuilt decoded stream; coverage over decoded execution
// is byte-identical + complete; the epoch/deps validity unit tests) live in the
// CRATE, not here, because they drive `pub(crate)` machinery — the in-crate
// `DebuggerHook` + command channel, `Vm::arm_coverage`, and `DecodedChunk`'s
// epoch/deps fields:
//   - `src/vm/run.rs` → `breakpoint_set_mid_hot_loop_invalidates_the_decoded_stream_and_fires`
//     (§8.4 #1) and `coverage_over_decoded_execution_is_byte_identical_and_complete` (§8.4 #3)
//   - `src/vm/decode.rs` → `decoded_chunk_validity_unit_tests` (§8.4 #4: own_epoch
//     invalid after set AND restore; a stale `deps` epoch invalidates a caller
//     stream whose own bytes are untouched — the cross-proto Unit-C hole)
// This file carries the PUBLIC-surface guards: the chokepoint source scan (below)
// and a coverage-over-a-decode-hot-loop black-box equivalence (a `--coverage`
// run patches `Op::Break` per line — bumping `patch_epoch` — so decode must
// invalidate + rebuild; the trap+un-patch+re-decode cycle must leave output
// byte-identical to an un-instrumented run).
// ─────────────────────────────────────────────────────────────────────────────

/// §8.4 #3 (public-surface black box): `--coverage` of a loop hot enough to decode
/// must produce output byte-identical to a plain VM run. Coverage patches each
/// line's first byte to `Op::Break` (each bumps `patch_epoch`); the decoded stream
/// is invalidated + rebuilt around every set/restore, and the observation-only
/// trap leaves stdout untouched. (The covered-set completeness + the decoded_ops>0
/// anti-false-green live in the in-crate `coverage_over_decoded_execution_*` test;
/// here we assert the public contract: coverage never changes what the program
/// prints, even when decode is engaged.)
#[tokio::test]
async fn coverage_over_a_decode_hot_loop_is_byte_identical_to_a_plain_run() {
    let src = "fn sq(n) {\n  return n * n\n}\nlet total = 0\n\
               for (i in 0..200) {\n  total = total + sq(i)\n}\nprint(total)\n";
    let plain = ascript::vm_run_source(src).await.expect("plain ok");
    let covered = ascript::vm_run_source_coverage(src).await.expect("coverage ok");
    assert_eq!(
        plain, covered,
        "coverage stdout is byte-identical to a plain run even when the loop decodes"
    );
    assert_eq!(plain.0, "2646700\n", "sum of i*i for i in 0..200");
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE Task 10 — Unit D: top-of-stack register cache + flush-edge battery (§7)
//
// One named test per §7.2 flush edge, each engineered to cross its edge with a
// LIVE cached TOS, asserting decoded-forced (TOS active) == decoded-no-tos ==
// tree-walker. A missed flush is a wrong-VALUE bug (not a crash) — exactly the
// class these tests + the per-edge sabotage (Task 10 Step 4) exist to catch.
// The dap/profile breakpoint edge (edge 1 × the §8.4 battery) lives in-crate
// (`src/vm/run.rs`) because it drives `pub(crate)` machinery.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tos_cached_bursts_execute_and_are_counted() {
    // Anti-false-green (spec §8.3e): TOS-cached records must actually retire.
    let st = ascript::vm_run_source_decode_stats(
        "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)")
        .await
        .expect("ok");
    assert_eq!(st.output, "499999500000\n");
    assert!(st.tos_ops > 0, "no TOS-cached records retired — Unit D is dead");
    // The dedicated kill switch kills EXACTLY Unit D:
    let st2 = ascript::vm_run_source_decode_stats_no_tos(
        "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)")
        .await
        .expect("ok");
    assert_eq!(st2.output, st.output);
    assert_eq!(st2.tos_ops, 0);
    assert!(st2.decoded_ops > 0, "no-tos must not kill decoding");
}

/// §8.3e: the no-tos differential mode joins the batteries — `decoded_no_tos`
/// retires records (`decoded_ops > 0`) but caches no TOS (`tos_ops == 0`), while
/// forced-with-TOS retires the SAME records with the cache active (`tos_ops > 0`).
#[tokio::test]
async fn decoded_no_tos_disables_only_the_tos_cache() {
    let src = "let s = 0\nfor (i in 0..10000) { s = s + i * 2 }\nprint(s)";
    // Stats with TOS active (forced, threshold 0).
    let on = ascript::vm_run_source_decode_stats(src).await.expect("ok");
    assert!(on.decoded_ops > 0 && on.tos_ops > 0, "TOS active: {on:?}");
    // Output equivalence across the three TOS-relevant modes.
    let forced = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let no_tos = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(forced, no_tos, "TOS on vs off");
    assert_eq!(tw, forced.0, "tree-walker vs decoded");
}

#[tokio::test]
async fn flush_edge_1_escalation_mid_expression() {
    // Edge 1 (escalation): a pending await whose Future is itself the cached TOS.
    // `await f` reads the local `f` into TOS, then the burst escalates (the future
    // is pending) — the async driver re-decodes Op::Await and must re-peek the
    // future on fiber.stack. A missed flush strands it in the cache. The `total +`
    // keeps another operand live below. Holding `f` across an awaited gap (a real
    // async fn) forces the pending branch.
    let src = r#"
async fn slow(x) { return x * 2 }
let total = 0
for (i in 0..200) {
  let f = slow(i)
  total = total + await f
}
print(total)
"#;
    let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(tw, on.0);
}

#[tokio::test]
async fn flush_edge_2_yield_suspends_with_a_complete_stack() {
    // Edge 2: a generator yielding mid-expression-rich body; the suspended fiber
    // is re-entered by resume — its stack must be complete.
    let src = r#"
fn* squares(n) { for (i in 0..n) { yield i * i + (i + 1) } }
let total = 0
for await (v in squares(50)) { total = total + v }
print(total)
"#;
    let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(tw, on.0);
}

#[tokio::test]
async fn flush_edge_3_fused_record_reads_under_a_cached_tos() {
    // Edge 3: a FUSED field-read-then-add (`GetProp v; Add`) runs while a PRIOR
    // value (`acc`) is cached in TOS. `exec_fused` pops its operands DIRECTLY off
    // `fiber.stack`; the flush before it must spill the cached operand or the fused
    // executor reads a stale stack (the per-edge sabotage strands `acc` in the cache
    // → a wrong sum). `acc = acc + o.v + i` keeps `acc` hot and fuses the field read.
    // Byte-identical: decoded-forced (TOS on) == no-tos == tree-walker.
    let src = r#"
let o = { v: 1000000 }
let acc = 5
for (i in 0..30) { acc = acc + o.v + i }
print(acc)
"#;
    let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(tw, on.0);

    // The panic-in-a-fused-record half: an i64 overflow INSIDE the fused field-add,
    // recovered — the rendered error must also be byte-identical across modes.
    let panic_src = r#"
fn run() {
  let o = { v: 9223372036854775807 }
  let acc = 5
  for (i in 0..40) { acc = acc + o.v + i }
  return acc
}
let [v, e] = recover(() => run())
print(v, e)
"#;
    let pon = ascript::vm_run_source_decoded_forced(panic_src).await.expect("ok");
    let poff = ascript::vm_run_source_decoded_no_tos(panic_src).await.expect("ok");
    let ptw = ascript::run_source(panic_src).await.expect("ok");
    assert_eq!(pon, poff);
    assert_eq!(ptw, pon.0);
}

#[tokio::test]
async fn flush_edge_4_call_at_cached_tos_state() {
    // Edge 4: a plain call whose LAST ARG is the cached TOS — check_call_args and
    // the callee window must see it physically on the stack. Also covers
    // InlineEnter (the inlined variant of the same callee).
    let src = r#"
fn add(a, b) { return a + b }
let s = 0
for (i in 0..100000) { s = add(s, i * 2 + 1) }
print(s)
"#;
    let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(tw, on.0);
}

#[tokio::test]
async fn flush_edge_5_return_into_a_caller_mid_expression() {
    // Edge 5 (frame push/pop): the caller holds a cached partial (`1000 + …`) in TOS
    // while the small global fn `f` is dispatched (inlined → the InlineEnter frame
    // push/pop path that runs the spliced body + returns its result on the same
    // lane). The flush before the inline dispatch must spill the caller's cached
    // partial so the callee's GET_GLOBAL + args window onto the right stack and the
    // returned result composes with the caller's flushed cache; the per-edge sabotage
    // strands the cached `1000` → a wrong sum. Byte-identical across modes.
    let src = r#"
fn f(n) { return n * 3 }
let total = 0
for (i in 0..100000) { total = total + (1000 + f(i)) }
print(total)
"#;
    let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
    let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(tw, on.0);
}

/// Under-TOS arms (the `peek(1)`-class builder/swap family) must read through the
/// accessor correctly when TOS is occupied: array/object/map builders, spreads,
/// SetIndex, and the Swap/Rot3 stack shuffles — all with a live cached operand.
#[tokio::test]
async fn under_tos_builder_and_shuffle_arms_are_byte_identical() {
    for src in [
        // AppendArray with a live TOS underneath (the array), spread of an array.
        "let base = 7\nlet a = [base, base + 1, base * 2]\nlet b = [...a, base + 3]\nprint(a, b)",
        // MapEntry / object builder with computed values under a cached operand.
        "let k = 2\nlet m = { x: k + 1, y: k * 10 }\nlet o = { ...m, z: k }\nprint(m, o)",
        // SetIndex with the value cached as TOS over the index+obj below.
        "let arr = [0, 0, 0]\nfor (i in 0..3) { arr[i] = i * i + 1 }\nprint(arr)",
        // Swap/Rot3-heavy: chained comparisons + index assignment in a loop.
        "let g = { c: 0 }\nfor (i in 0..50) { g.c = g.c + (i % 3) }\nprint(g.c)",
    ] {
        let on = ascript::vm_run_source_decoded_forced(src).await.expect("ok");
        let off = ascript::vm_run_source_decoded_no_tos(src).await.expect("ok");
        let tw = ascript::run_source(src).await.expect("ok");
        assert_eq!(on, off, "TOS on vs off `{src}`");
        assert_eq!(tw, on.0, "tree-walker vs decoded `{src}`");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE §4.1 — the invalidation chokepoint structural guard (Task 1)
// ─────────────────────────────────────────────────────────────────────────────

/// DECODE §4.1: `Code::patch_byte` (the raw UnsafeCell write) must be reachable
/// ONLY through `Chunk::patch_byte` (which bumps `patch_epoch`). A future patch
/// site calling the raw Code method would silently skip invalidation — this
/// source scan trips on it. (The behavioral proof is the Task-6 battery; this
/// is the cheap structural guard.)
#[test]
fn raw_code_patch_byte_has_no_callers_outside_chunk_rs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    visit(&root, &mut |path, text| {
        if path.ends_with("vm/chunk.rs") {
            return; // the definition + the one sanctioned caller
        }
        for (i, line) in text.lines().enumerate() {
            // `chunk.patch_byte(`/-style calls are fine (they bump); the raw form is
            // `code.patch_byte(` / `.code.patch_byte(` — flag those.
            if line.contains("code.patch_byte(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "raw Code::patch_byte callers bypass patch_epoch:\n{}",
        offenders.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE Task 9 — Unit C: speculative global-fn inlining (public-API behavior)
//
// The two INTERNAL behavioral halves (a breakpoint inside an inlined callee
// invalidating the caller's stream via `deps`; an armed profiler disabling inline
// segments) need VM internals (`Vm::with_all_flags`, `instrument`), so they live in
// `src/vm/run.rs`'s test module beside the Task-6 battery:
//   - `breakpoint_inside_an_inlined_callee_invalidates_the_caller_stream`
//   - `profiler_armed_disables_inline_segments_only`
// ─────────────────────────────────────────────────────────────────────────────

/// §6.1/§6.2: a small untyped global `fn` called in a hot loop is inlined and the
/// `struct_gen`+identity guard HITS on every iteration; the dedicated toggle
/// (`ASCRIPT_NO_DECODE_INLINE`) kills exactly this (no inline hits) while leaving
/// the decoded path (and Unit B fusion) running.
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

/// §6.2: BOTH guard legs deopt byte-identically. (i) a `struct_gen` miss (a NEW
/// top-level define after the warm loop bumps the gen); (ii) an IDENTITY miss (a
/// mutable `let` global rebound to a different closure — `update_user_global` does
/// NOT bump `struct_gen`, so the `Rc::ptr_eq` identity leg is what catches it).
#[tokio::test]
async fn guard_miss_struct_gen_and_identity_both_fall_back_byte_identically() {
    let gen_miss = r#"
fn add(a, b) { return a + b }
let s = 0
for (i in 0..100000) { s = add(s, i) }
let unrelated = 1
for (i in 0..100000) { s = add(s, i) }
print(s + unrelated)
"#;
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
        assert_eq!(tw, st.output_exit().0, "tw vs decoded on `{src}`");
        assert_eq!(st.output_exit(), off, "decoded vs byte on `{src}`");
        assert!(st.inline_misses > 0, "the miss path never ran on `{src}`");
    }
}

/// SP3 §B byte-identity: deep recursion THROUGH an inline-eligible leaf at the
/// boundary. `leaf` is inline-eligible at its call site inside `deep`; `deep` is
/// recursive (not inlineable). The depth panic must be byte-identical in all three
/// modes — proving `InlineEnter` bumps the depth counter exactly ONCE per logical
/// call.
#[tokio::test]
async fn inlined_call_counts_one_logical_call_depth_unit() {
    let src = r#"
fn leaf(a) { return a + 1 }
fn deep(n) { return deep(leaf(n)) }
print(deep(0))
"#;
    let tw = ascript::run_source(src).await.expect_err("panics").to_string();
    let on = ascript::vm_run_source_decoded_forced(src).await.expect_err("panics").to_string();
    let off = ascript::vm_run_source_no_decode(src).await.expect_err("panics").to_string();
    assert_eq!(on, off);
    assert_eq!(on, tw);
    assert!(on.contains("maximum recursion depth exceeded"));
}

/// §6.4: a panic inside an inlined body anchors at the CALLEE-chunk offset; the
/// rendered error (message + caret) equals the no-decode run. An arithmetic type
/// panic inside the leaf body.
#[tokio::test]
async fn panic_inside_an_inlined_body_keeps_the_callee_span_and_source() {
    let src = "fn bad(a) { return a + \"x\" }\nlet s = 0\nfor (i in 0..100000) { s = bad(s) }\nprint(s)";
    let on = ascript::vm_run_source_decoded_forced(src).await.expect_err("panics");
    let off = ascript::vm_run_source_no_decode(src).await.expect_err("panics");
    assert_eq!(on.to_string(), off.to_string());
}







