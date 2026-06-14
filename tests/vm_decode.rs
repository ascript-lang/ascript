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
    // wired counters are positive; the Unit B/C/D counters stay 0.
    let st = ascript::vm_run_source_decode_stats(src).await.expect("stats ok");
    assert!(st.decoded_ops > 0, "RecordSource must retire records under forced decode");
    assert!(st.decoded_bytes > 0, "memory accounting must report");
    assert!(st.stack_ops > 0, "stack-traffic gate input must count from the first record");
    assert_eq!(
        (st.fused_ops, st.inline_hits, st.inline_misses, st.tos_ops),
        (0, 0, 0, 0),
        "Unit B/C/D counters stay 0 until their tasks land"
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
