//! DECODE §5.1 (Unit B part 1): the decoded-dispatch PAIR/TRIPLE census harness.
//!
//! This `#[ignore]`d test runs the curated `bench/profiling/*.as` programs PLUS the
//! runnable example corpus (`examples/**`) in FORCED-DECODE census mode (every proto
//! decoded so the real record stream is seen), aggregates the dynamic `(prev, op)`
//! pair and `(prev2, prev, op)` triple counts WITHIN BASIC BLOCKS, and prints the
//! ranked table (dynamic counts + % of total records).
//!
//! The ranked output is the MEASURED data Task 8 fuses into superinstructions — the
//! DECODE spec mandates fusion pairs be chosen from data, never guessed. Run it with:
//!
//! ```text
//! cargo test --release --features decode-census --test decode_census -- --ignored --nocapture
//! ```
//!
//! The whole harness is `#[cfg(feature = "decode-census")]`; without the feature this
//! file compiles to nothing, so a default `cargo test` never builds the counting path.

#![cfg(feature = "decode-census")]

use ascript::vm::opcode::Op;
use ascript::CENSUS_NO_PREV;
use std::collections::HashMap;

/// A dedicated big-stack thread + its own current-thread tokio runtime (the
/// interpreter is `!Send`), mirroring `tests/vm_bench.rs:374-392`. Deep recursion
/// in the fib-style corpus under the async engine needs far more than the 2 MiB
/// default test-thread stack.
const WORKER_STACK_BYTES: usize = 256 * 1024 * 1024;

/// Files that BLOCK forever (a `serve`/accept loop) or that need a real on-disk
/// sibling for a relative import — they cannot run headless to completion here.
/// Mirrors `tests/vm_differential.rs`'s `LongRunningServer`/`RelativeImports` skips.
/// (Nondeterministic examples are NOT skipped: the census never compares output, so
/// random/time/network bytes are harmless — they still produce a real record stream.)
const SKIP: &[&str] = &[
    "examples/advanced/http_server.as",
    "examples/advanced/ws_server.as",
    "examples/advanced/server_multicore.as",
    "examples/advanced/bundle_caps.as",
    "examples/bundle_multimodule.as",
];

/// Enumerate the runnable example corpus (the same set the differential oracle
/// walks), relative to the crate root.
fn all_corpus_examples() -> Vec<String> {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut out = Vec::new();
    for dir in ["examples", "examples/advanced"] {
        let p = std::path::Path::new(root).join(dir);
        let rd = std::fs::read_dir(&p).unwrap_or_else(|e| panic!("read_dir {dir}: {e}"));
        for entry in rd {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|x| x.to_str()) == Some("as") {
                out.push(format!("{dir}/{}", path.file_name().unwrap().to_string_lossy()));
            }
        }
    }
    out.sort();
    out
}

/// Enumerate `bench/profiling/*.as`.
fn profiling_programs() -> Vec<String> {
    let root = env!("CARGO_MANIFEST_DIR");
    let p = std::path::Path::new(root).join("bench/profiling");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&p) {
        for entry in rd {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|x| x.to_str()) == Some("as") {
                out.push(format!("bench/profiling/{}", path.file_name().unwrap().to_string_lossy()));
            }
        }
    }
    out.sort();
    out
}

/// The readable name of an op discriminant (`{:?}` of the decoded `Op`).
fn op_name(disc: u16) -> String {
    match u8::try_from(disc).ok().and_then(Op::from_u8) {
        Some(op) => format!("{op:?}"),
        None => format!("op#{disc}"),
    }
}

#[test]
#[ignore = "census harness — run explicitly: cargo test --release --features decode-census --test decode_census -- --ignored --nocapture"]
fn decode_pair_triple_census() {
    std::thread::Builder::new()
        .name("decode-census".into())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");
            rt.block_on(run_census());
        })
        .expect("spawn census thread")
        .join()
        .expect("census thread panicked");
}

async fn run_census() {
    let root = env!("CARGO_MANIFEST_DIR");

    // The program set: the curated profiling programs FIRST (the perf-shaped hot
    // loops), then the whole example corpus minus the blocking/relative-import set.
    let mut programs: Vec<String> = profiling_programs();
    for rel in all_corpus_examples() {
        if !SKIP.contains(&rel.as_str()) {
            programs.push(rel);
        }
    }

    // Global aggregate over every program's drained census.
    let mut counts: HashMap<(u16, u16, u16), u64> = HashMap::new();
    let mut total_records: u64 = 0;
    let mut ran = 0usize;
    let mut skipped_err = 0usize;

    for rel in &programs {
        let path = std::path::Path::new(root).join(rel);
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  (skip {rel}: read error {e})");
                continue;
            }
        };
        match ascript::vm_run_source_census(&src).await {
            Ok((c, total)) => {
                for (k, v) in c {
                    *counts.entry(k).or_insert(0) += v;
                }
                total_records += total;
                ran += 1;
            }
            Err(e) => {
                // A feature-unavailable import / a program that genuinely errors:
                // skip it (the census never needs a clean exit, only a record stream).
                eprintln!("  (skip {rel}: {})", e.message);
                skipped_err += 1;
            }
        }
    }

    // Split the aggregate into pairs (slot0 == sentinel) and triples.
    let mut pairs: Vec<((u16, u16), u64)> = Vec::new();
    let mut triples: Vec<((u16, u16, u16), u64)> = Vec::new();
    for (&(s0, p, o), &n) in &counts {
        if s0 == CENSUS_NO_PREV {
            pairs.push(((p, o), n));
        } else {
            triples.push(((s0, p, o), n));
        }
    }
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    triples.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let denom = total_records.max(1) as f64;

    println!();
    println!("=== DECODE §5.1 PAIR/TRIPLE CENSUS ===");
    println!(
        "programs run = {ran} (skipped {} blocking/relative; {skipped_err} errored)",
        SKIP.len()
    );
    println!("total records retired (within basic blocks) = {total_records}");
    println!();

    println!("--- TOP 40 PAIRS (prev → op) ---");
    println!("{:>14}  {:>7}   pair", "count", "%recs");
    for ((p, o), n) in pairs.iter().take(40) {
        println!(
            "{:>14}  {:>6.3}%   {} -> {}",
            n,
            (*n as f64) / denom * 100.0,
            op_name(*p),
            op_name(*o),
        );
    }
    println!();

    println!("--- TOP 40 TRIPLES (prev2 → prev → op) ---");
    println!("{:>14}  {:>7}   triple", "count", "%recs");
    for ((p2, p, o), n) in triples.iter().take(40) {
        println!(
            "{:>14}  {:>6.3}%   {} -> {} -> {}",
            n,
            (*n as f64) / denom * 100.0,
            op_name(*p2),
            op_name(*p),
            op_name(*o),
        );
    }
    println!();

    // Anti-false-green: the census must have SEEN a real record stream and counted
    // pairs/triples. A zero here means the forced-decode driver never ran or the
    // basic-block reset wiped everything — a harness/wiring bug, not valid data.
    assert!(ran > 0, "no programs ran in census mode");
    assert!(total_records > 0, "the census saw zero retired records (forced-decode off?)");
    assert!(!pairs.is_empty(), "no pairs counted — basic-block tracking is broken");
    assert!(!triples.is_empty(), "no triples counted — basic-block tracking is broken");
}

/// PROBE (reviewer checkpoint): the basic-block reset is load-bearing — a pair must
/// NEVER be counted across a jump target (that would suggest an ILLEGAL fusion). A
/// crafted two-block program (a forward branch) is run in census mode; we assert the
/// op that STARTS the second block (the branch target) is never recorded as the
/// successor of the op that ENDS the first block (the branch test / the jump).
///
/// `if (cond) {A} else {B}` lowers to two blocks joined by a `JUMP_IF_FALSE`/`JUMP`
/// pair; the join point opens a fresh block. We assert NO pair has a jump op as its
/// `prev` (a terminator resets the window, so the target op opens a new block with
/// `prev = None`), proving no record straddles the boundary.
#[tokio::test]
async fn census_basic_block_reset_no_pair_across_a_jump() {
    // Two basic blocks separated by a branch. The `print` calls and arithmetic give
    // the blocks real bodies so a straight-line pair WITHIN a block is recorded
    // (proving the census is live) while NO pair crosses the branch.
    let src = r#"
let x = 0
if (x < 1) {
  let a = 1 + 2
  print(a)
} else {
  let b = 3 + 4
  print(b)
}
let y = 5 + 6
print(y)
"#;
    let (counts, total) = ascript::vm_run_source_census(src)
        .await
        .expect("census run completes");
    assert!(total > 0, "the program retired records");

    // Collect the jump/branch/terminator op discriminants.
    let jump_ops: Vec<u16> = [
        Op::Jump,
        Op::JumpIfFalse,
        Op::JumpIfTrue,
        Op::JumpIfNotNil,
        Op::Loop,
        Op::JumpIfArgSupplied,
    ]
    .iter()
    .map(|o| *o as u8 as u16)
    .collect();

    // NO pair may have a jump op as its `prev` slot — a jump TERMINATES its block,
    // so the op after it (in a different block) must open a fresh block with no
    // in-block predecessor. A counted `(jump, target)` pair would be a fusion that
    // crosses a basic-block boundary — exactly the illegal case the reset prevents.
    for (&(s0, prev, op), &n) in &counts {
        if s0 == CENSUS_NO_PREV && jump_ops.contains(&prev) {
            panic!(
                "a pair was counted ACROSS a jump boundary: {} -> {} (count {n}) — \
                 the basic-block reset failed",
                op_name(prev),
                op_name(op),
            );
        }
    }

    // Sanity: the census IS live — at least one in-block pair was recorded.
    let any_pair = counts.keys().any(|&(s0, _, _)| s0 == CENSUS_NO_PREV);
    assert!(any_pair, "the census recorded no pairs at all (harness wiring is dead)");
}
