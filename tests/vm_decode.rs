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

/// DECODE Task 2: the three kill switches and the stats test entry point exist,
/// their outputs are byte-identical to the default VM, and all stat counters are
/// 0 before Task 4 wires up the RecordSource driver.
///
/// This test is the INERT scaffolding gate — it must stay green at every future
/// task boundary until Task 4 lands (at which point `decoded_ops` rises above 0
/// and the Task-4 anti-false-green test takes over).
#[tokio::test]
async fn decode_entry_points_exist_and_are_inert_pre_driver() {
    let src = "let s = 0\nfor (i in 0..100) { s = s + i }\nprint(s)";
    let on = ascript::vm_run_source(src).await.expect("default ok");
    let off = ascript::vm_run_source_no_decode(src).await.expect("no-decode ok");
    let forced = ascript::vm_run_source_decoded_forced(src).await.expect("forced ok");
    assert_eq!(on, off, "decode-off must be byte-identical to default");
    assert_eq!(on, forced, "decode-forced must be byte-identical to default");
    // Pre-driver, every counter reads 0 in every mode.
    let st = ascript::vm_run_source_decode_stats(src).await.expect("stats ok");
    assert_eq!(
        (st.decoded_ops, st.fused_ops, st.inline_hits, st.inline_misses,
         st.decoded_bytes, st.stack_ops, st.tos_ops),
        (0, 0, 0, 0, 0, 0, 0),
        "all DECODE counters must be 0 pre-Task-4"
    );
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
