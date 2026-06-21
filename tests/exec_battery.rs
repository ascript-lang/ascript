//! EXEC Task 9 — the bespoke-executor invariant battery (spec §6.4, §4 S8/S9/S11/S12/S14).
//!
//! EXEC replaced tokio's task harness with a `!Send` bespoke per-isolate executor,
//! now the DEFAULT driver (`ASCRIPT_EXECUTOR=tokio` is the permanent kill switch;
//! [`ascript::vm_run_source_tokio_exec`] is the executor-off test entry). This file
//! proves the structured-concurrency invariants HOLD under the bespoke executor —
//! it is TEST-ONLY (zero engine change).
//!
//! - **S-mem (§6.4) — THE leak gate.** An un-awaited async loop (each call cancelled
//!   on drop) stays bounded: the in-flight high-water mark does NOT scale with the
//!   iteration count (cancel-on-drop + the slab free-list reclaim work), and the
//!   process RSS stays in the ~8 MB class — NOT the 130 MB-class leak the M17 ADR
//!   eliminated. This SETTLES the libFuzzer exit-OOM question: a per-spawn executor
//!   leak would grow RSS monotonically; a single pathological generated program
//!   would not.
//! - **S8** — the inflight cap reaps > 256 un-awaited tasks (byte-identical bespoke
//!   vs tokio).
//! - **S9** — generators are consumer-driven, NOT spawned tasks → the executor never
//!   touches them (`src/coro.rs` has ZERO diff on this branch; verified by
//!   `git diff main...HEAD --stat -- src/coro.rs`).
//! - **S11** — `run_scoped_root`'s `exec.drain()` runs detached survivors like
//!   `local.await` (a `task.spawn`'d side effect completes post-root).
//! - **S12** — task-locals (telemetry spans) are future-wrappers → executor-agnostic
//!   per-task isolation.
//! - **S14** — the recursion-depth guard (clean "maximum recursion depth exceeded",
//!   never SIGABRT) operates inside the body future regardless of who polls it.

#![cfg(not(ascript_rt))]

use std::process::Command;

// =============================================================================
// §S-mem — THE M17 leak gate (spec §6.4)
// =============================================================================

/// The un-awaited-async-loop program: an `async fn` called in a loop WITHOUT
/// awaiting or holding the returned `future<T>`. Each call's handle is dropped
/// immediately → the backing task is cancelled-on-drop. Without reaping, the
/// in-flight high-water mark (and RSS) would grow toward `iters`.
fn unawaited_loop_program(iters: usize) -> String {
    format!(
        "async fn work(n) {{ return n }}\n\
         let i = 0\n\
         while (i < {iters}) {{\n  work(i)\n  i = i + 1\n}}\n\
         print(\"done\")\n"
    )
}

/// In-process inflight bound under the BESPOKE executor (the default).
/// `vm_run_source_with_interp` drives the root through `exec::run_scoped_root`
/// (the bespoke install chokepoint), so the returned interp's `max_inflight()`
/// high-water mark is the bespoke executor's. A 20 000-iteration un-awaited loop
/// must stay FAR below the iteration count — the `INFLIGHT_YIELD_CAP` (256) class,
/// not O(iters). This proves cancel-on-drop + the slab free-list reclaim under
/// bespoke; without reaping, the peak would approach 20 000.
#[tokio::test]
async fn inflight_bounded_under_bespoke() {
    let (out, interp) = ascript::vm_run_source_with_interp(&unawaited_loop_program(20_000))
        .await
        .expect("program runs under the bespoke executor");
    assert_eq!(out, "done\n");
    let peak = interp.max_inflight();
    assert!(
        peak < 1000,
        "in-flight high-water mark must stay bounded (≈256 class, NOT O(iters)); \
         got {peak} for a 20000-iteration un-awaited loop — a value near 20000 \
         would mean cancel-on-drop / slab reclaim is NOT working under bespoke"
    );
}

/// CLI-level RSS leak proof (the definitive check). Runs the binary on an
/// un-awaited-async-loop at a LARGE iteration count under `/usr/bin/time -l`,
/// parses "maximum resident set size", and asserts the peak stays under a
/// generous machine-tolerant ceiling. The regression it guards is the 130 MB-class
/// leak (the M17 ADR's pre-fix number). Run under BOTH the default (bespoke) AND
/// `ASCRIPT_EXECUTOR=tokio` so the bespoke executor is proven no worse than tokio.
///
/// `/usr/bin/time -l` (and its byte-valued "maximum resident set size" line) is
/// macOS-specific, so the RSS assertion is `cfg(target_os = "macos")`-gated; on
/// other platforms the test still runs the program (smoke) but skips the parse.
#[test]
fn bespoke_rss_stays_bounded() {
    let dir = std::env::temp_dir();
    let prog = dir.join(format!("ascript_exec_rss_{}.as", std::process::id()));
    std::fs::write(&prog, unawaited_loop_program(200_000)).expect("write temp program");

    // A generous ceiling: the measured ~14 MB has huge headroom; the guarded
    // regression is the 130 MB class. 40 MB is machine-tolerant and still catches
    // a real leak by a wide margin.
    const RSS_CEILING_BYTES: u64 = 40 * 1024 * 1024;

    for executor in ["bespoke", "tokio"] {
        let mut cmd = Command::new("/usr/bin/time");
        cmd.arg("-l")
            .arg(env!("CARGO_BIN_EXE_ascript"))
            .arg("run")
            .arg(&prog);
        if executor == "tokio" {
            cmd.env("ASCRIPT_EXECUTOR", "tokio");
        }
        let output = match cmd.output() {
            Ok(o) => o,
            // `/usr/bin/time` missing (non-macOS / minimal env): skip cleanly.
            Err(_) => {
                eprintln!("skipping RSS check ({executor}): /usr/bin/time -l unavailable");
                continue;
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("done"),
            "{executor}: program did not complete (stdout: {stdout:?})"
        );

        // `time -l` writes its report to stderr.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let rss = parse_max_rss_bytes(&stderr);

        #[cfg(target_os = "macos")]
        {
            let rss = rss.unwrap_or_else(|| {
                panic!("{executor}: could not parse maximum resident set size from: {stderr}")
            });
            assert!(
                rss < RSS_CEILING_BYTES,
                "{executor}: peak RSS {rss} bytes ({:.1} MB) exceeds the {:.0} MB ceiling — \
                 the un-awaited async loop is NOT bounded (130 MB-class leak regression)",
                rss as f64 / (1024.0 * 1024.0),
                RSS_CEILING_BYTES as f64 / (1024.0 * 1024.0),
            );
            eprintln!(
                "exec RSS ({executor}): {:.1} MB at 200k un-awaited iters (ceiling {:.0} MB)",
                rss as f64 / (1024.0 * 1024.0),
                RSS_CEILING_BYTES as f64 / (1024.0 * 1024.0),
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Non-macOS `time -l` output shape differs; the program completing is
            // the portable signal. Use the value opportunistically if present.
            if let Some(rss) = rss {
                assert!(
                    rss < RSS_CEILING_BYTES,
                    "{executor}: peak RSS {rss} bytes exceeds the ceiling"
                );
            }
        }
        let _ = RSS_CEILING_BYTES;
    }
    let _ = std::fs::remove_file(&prog);
}

/// Parse the byte value from a `/usr/bin/time -l` "maximum resident set size" line.
/// On macOS the value is in bytes; the line looks like
/// `      14696448  maximum resident set size`.
fn parse_max_rss_bytes(stderr: &str) -> Option<u64> {
    stderr
        .lines()
        .find(|l| l.contains("maximum resident set size"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
}

// =============================================================================
// §S8 — inflight cap fairness: > 256 un-awaited tasks are reaped, bespoke == tokio
// =============================================================================

/// A program spawning MORE than 256 un-awaited side-effecting async calls. The
/// `maybe_yield_for_inflight` cap reaps in-flight tasks so the side effects all
/// land; the output must be byte-identical between the bespoke executor (default)
/// and the tokio executor — fairness is executor-invariant.
#[tokio::test]
async fn inflight_cap_reaps_under_bespoke() {
    // 400 > 256 (INFLIGHT_YIELD_CAP). Each call appends to a shared counter via a
    // side effect; we await a gather at the end so every side effect is observed,
    // and assert determinism by summing (order-independent).
    let src = "\
let total = 0\n\
async fn bump(n) { total = total + n }\n\
import { gather } from \"std/task\"\n\
let tasks = []\n\
let i = 0\n\
while (i < 400) {\n  tasks = [...tasks, bump(1)]\n  i = i + 1\n}\n\
await gather(tasks)\n\
print(total)\n";

    let (bespoke, _) = ascript::vm_run_source(src)
        .await
        .expect("bespoke run ok");
    let (tokio_exec, _) = ascript::vm_run_source_tokio_exec(src)
        .await
        .expect("tokio-executor run ok");
    assert_eq!(bespoke, "400\n", "all 400 un-awaited side effects must land");
    assert_eq!(
        bespoke, tokio_exec,
        "inflight-cap fairness must be byte-identical bespoke vs tokio"
    );
}

// =============================================================================
// §S9 — generators never touch the executor (consumer-driven, not spawned)
// =============================================================================

/// A generator corpus program produces identical output across bespoke, tokio,
/// and the tree-walker. Generators are consumer-driven coroutines (`src/coro.rs`),
/// NOT spawned tasks — the executor must never touch them. (Structural proof: this
/// branch's `git diff main...HEAD --stat -- src/coro.rs` is EMPTY, asserted in the
/// task report; here we prove behaviour is unchanged across executors.)
#[tokio::test]
async fn generators_untouched_under_bespoke() {
    let src = "\
fn* counter(limit) {\n  let i = 0\n  while (i < limit) {\n    yield i * i\n    i = i + 1\n  }\n}\n\
async fn* aticks(n) {\n  let j = 0\n  while (j < n) {\n    yield j\n    j = j + 1\n  }\n}\n\
for await (v in counter(5)) { print(v) }\n\
let acc = 0\n\
for await (t in aticks(4)) { acc = acc + t }\n\
print(acc)\n";

    let (bespoke, _) = ascript::vm_run_source(src).await.expect("bespoke ok");
    let (tokio_exec, _) = ascript::vm_run_source_tokio_exec(src)
        .await
        .expect("tokio-exec ok");
    let (tree, _interp) = ascript::run_source_with_interp(src)
        .await
        .expect("tree-walker ok");

    assert_eq!(bespoke, "0\n1\n4\n9\n16\n6\n");
    assert_eq!(
        bespoke, tokio_exec,
        "generator output must be identical bespoke vs tokio (generators are not tasks)"
    );
    assert_eq!(
        bespoke, tree,
        "generator output must be identical bespoke vs tree-walker"
    );
}

// =============================================================================
// §S11 — drain parity: a detached survivor finishes during the post-root drain
// =============================================================================

/// A `task.spawn`'d task (opted OUT of cancel-on-drop) whose side effect lands
/// AFTER the root completes — proving `run_scoped_root`'s `exec.drain()` runs
/// survivors exactly like tokio's `local.await`. The survivor line must appear
/// under BOTH the bespoke executor and the tokio executor.
#[tokio::test]
async fn drain_runs_detached_survivor_under_bespoke() {
    let src = "\
import { spawn } from \"std/task\"\n\
import { sleep } from \"std/time\"\n\
spawn(async () => {\n  await sleep(5)\n  print(\"survivor ran\")\n})\n\
print(\"root done\")\n";

    let (bespoke, _) = ascript::vm_run_source(src).await.expect("bespoke ok");
    let (tokio_exec, _) = ascript::vm_run_source_tokio_exec(src)
        .await
        .expect("tokio-exec ok");

    // Order between the root print and the drained survivor is scheduling-dependent;
    // assert by set membership (both lines present), not exact ordering.
    for (label, out) in [("bespoke", &bespoke), ("tokio", &tokio_exec)] {
        assert!(
            out.contains("root done"),
            "{label}: root print missing (out: {out:?})"
        );
        assert!(
            out.contains("survivor ran"),
            "{label}: detached survivor's side effect must be observed during the drain \
             (out: {out:?})"
        );
    }
    assert_eq!(
        bespoke.lines().count(),
        tokio_exec.lines().count(),
        "bespoke and tokio drains must observe the same number of lines"
    );
}

// =============================================================================
// §S12 — task-local (telemetry span) isolation is executor-agnostic
// =============================================================================

/// Two concurrent `task.spawn`'d telemetry scopes, each opening a child span, must
/// each parent THEIR child to THEIR own span — never cross — under the bespoke
/// executor. Task-locals are future-wrappers (executor-agnostic); this is the
/// adversarial interleave from `tests/telemetry.rs`
/// (`concurrent_scoped_spans_do_not_cross_parent`) re-asserted on the default
/// (bespoke) driver via the span-buffer seam.
#[cfg(feature = "telemetry")]
#[tokio::test]
async fn telemetry_span_isolation_under_bespoke() {
    let src = "\
import * as telemetry from \"std/telemetry\"\n\
let [ok, err] = telemetry.init({\n  service: \"exec-test\",\n  exporters: [],\n})\n\
import { sleep } from \"std/time\"\n\
import { spawn, gather } from \"std/task\"\n\
fn worker(tag, first, second) {\n\
  return telemetry.span(tag, async () => {\n\
    await sleep(first)\n\
    let child = telemetry.startSpan(tag + \"-child\")\n\
    await sleep(second)\n\
    child.end()\n\
  })\n\
}\n\
let a = spawn(async () => { await worker(\"A\", 2, 20) })\n\
let b = spawn(async () => { await worker(\"B\", 4, 4) })\n\
await gather([a, b])\n";

    // run_source_with_interp / vm_run_source_with_interp both drive through the
    // bespoke executor (run_scoped_root). Use the VM entry so it is the production
    // (specialized-VM + bespoke) configuration.
    let (_out, interp) = ascript::vm_run_source_with_interp(src)
        .await
        .expect("telemetry program runs under bespoke");
    let spans = interp.telemetry_spans_debug();
    let find = |name: &str| {
        spans
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing span {name}: {:?}", spans.iter().map(|s| &s.name).collect::<Vec<_>>()))
    };
    let a = find("A");
    let a_child = find("A-child");
    let b = find("B");
    let b_child = find("B-child");
    assert_eq!(
        a_child.parent_id.as_deref(),
        Some(a.span_id.as_str()),
        "A-child must parent to A under bespoke (no cross-task leak)"
    );
    assert_eq!(
        b_child.parent_id.as_deref(),
        Some(b.span_id.as_str()),
        "B-child must parent to B under bespoke (no cross-task leak)"
    );
    assert_ne!(a.trace_id, b.trace_id, "A and B are distinct traces");
}

// =============================================================================
// §S14 — recursion-depth guard holds under bespoke (clean panic, not SIGABRT)
// =============================================================================

/// Unbounded recursion in an `async fn` (so the body runs inside a future the
/// bespoke executor polls) must raise the clean Tier-2 "maximum recursion depth
/// exceeded" panic — never SIGABRT. The `call_depth` / `grow_future` machinery
/// operates inside the body future regardless of who polls it. (The broader
/// no-SIGABRT CLI battery lives in `tests/vm_limits.rs`, which now defaults to the
/// bespoke executor; this is the focused executor-tagged assertion.)
#[tokio::test]
async fn recursion_depth_under_bespoke() {
    let src = "\
async fn deep(n) { return await deep(n + 1) }\n\
await deep(0)\n";

    let err = ascript::vm_run_source(src)
        .await
        .expect_err("unbounded async recursion must error, not abort");
    assert!(
        err.message.contains("maximum recursion depth exceeded"),
        "expected the clean recursion-depth panic under bespoke, got: {}",
        err.message
    );
}
