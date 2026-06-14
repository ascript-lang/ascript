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
    // fusion fires → `fused_ops > 0`. The Unit C/D counters stay 0.
    let st = ascript::vm_run_source_decode_stats(src).await.expect("stats ok");
    assert!(st.decoded_ops > 0, "RecordSource must retire records under forced decode");
    assert!(st.decoded_bytes > 0, "memory accounting must report");
    assert!(st.stack_ops > 0, "stack-traffic gate input must count from the first record");
    assert!(st.fused_ops > 0, "Unit B fusion must fire on the accumulate loop");
    assert_eq!(
        (st.inline_hits, st.inline_misses, st.tos_ops),
        (0, 0, 0),
        "Unit C/D counters stay 0 until their tasks land"
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
