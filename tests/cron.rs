//! BATT D1 (§11) regression: `std/cron` opts/schedule parsing must use the
//! slab-safe `ObjectCell::get` accessor, NOT `borrow()` (which PANICS in slab
//! mode). The VM builds source-literal objects in SLAB storage; the tree-walker
//! builds DICT storage. A `borrow()` on the opts/schedule Object therefore aborts
//! the VM while the tree-walker succeeds — a four-mode byte-identity divergence
//! AND an uncatchable VM crash. These tests run a SOURCE-LITERAL opts object on
//! BOTH engines and assert byte-identical, panic-free output.
//!
//! The existing 27 in-module unit tests build opts via `IndexMap` (Dict storage),
//! so they cannot catch this — only a VM run over real source can.

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Run `src` through the binary on the given engine, returning (stdout, success).
fn run(src: &str, tree_walker: bool) -> (String, bool, String) {
    let bin = env!("CARGO_BIN_EXE_ascript");
    // A per-call unique path so parallel tests never clobber each other's file.
    let file = std::env::temp_dir().join(format!(
        "ascript_cron_{}_{}_{}.as",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        if tree_walker { "tw" } else { "vm" }
    ));
    std::fs::write(&file, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.arg("run").arg(&file);
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_file(&file);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `cron.next` with a SOURCE-LITERAL opts object must succeed on the VM (no slab
/// panic) and match the tree-walker exactly.
#[test]
fn next_with_literal_opts_object_no_slab_panic_four_mode() {
    let src = "import * as cron from \"std/cron\"\n\
        let r = cron.next(\"*/15 * * * *\", {after: 1767269220000, tzOffset: 0})\n\
        print(r[0])\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(
        vm_ok,
        "VM aborted on a source-literal opts object (slab borrow panic):\n{vm_err}"
    );
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on opts call");
    // The next */15 boundary strictly after 1767269220000 (2026-01-01T12:07:00Z)
    // is 12:15:00Z = 1767269700000 ms.
    assert_eq!(vm_out.trim(), "1767269700000.0");
}

/// `cron.matches` with a literal `{tzOffset}` opts object (the field-shift path).
#[test]
fn matches_with_literal_tz_offset_opts_four_mode() {
    let src = "import * as cron from \"std/cron\"\n\
        let r = cron.matches(\"0 0 * * *\", 1767312000000, {tzOffset: -300})\n\
        print(r[0])\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(vm_ok, "VM aborted on cron.matches literal opts:\n{vm_err}");
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on matches opts");
}

/// `cron.nextN` with a literal opts object (also routes through `read_opts`).
#[test]
fn next_n_with_literal_opts_object_four_mode() {
    let src = "import * as cron from \"std/cron\"\n\
        let r = cron.nextN(\"0 0 * * *\", 2, {after: 1767225600000, tzOffset: 0})\n\
        print(r[0])\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(vm_ok, "VM aborted on cron.nextN literal opts:\n{vm_err}");
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on nextN opts");
}

/// `cron.schedule` with a literal `{tzOffset}` opts object (the `read_opts` path
/// inside `cron_schedule`). The job is stopped immediately so the program exits.
#[test]
fn schedule_with_literal_opts_object_no_slab_panic_four_mode() {
    let src = "import * as cron from \"std/cron\"\n\
        let [job, err] = cron.schedule(\"*/5 * * * *\", () => print(\"tick\"), {tzOffset: 0})\n\
        job.stop()\n\
        print(job.running())\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(
        vm_ok,
        "VM aborted on cron.schedule literal opts (slab borrow panic):\n{vm_err}"
    );
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on schedule opts");
    assert_eq!(vm_out.trim(), "false", "stop() must clear running()");
}

/// A user-built schedule-OBJECT literal passed back into `cron.next` exercises
/// `resolve_schedule` / `schedule_from_object`, which read the object. A
/// previously-parsed schedule round-trips: `cron.parse` builds the tagged object,
/// and feeding it back into `cron.next` must read its bitmask fields slab-safely
/// (no `borrow()` abort) and produce the SAME next time on BOTH engines.
#[test]
fn parsed_schedule_object_round_trips_into_next_four_mode() {
    let src = "import * as cron from \"std/cron\"\n\
        let [sched, perr] = cron.parse(\"*/15 * * * *\")\n\
        let [ms, nerr] = cron.next(sched, {after: 1767269220000, tzOffset: 0})\n\
        print(ms)\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, tw_err) = run(src, true);
    assert!(
        vm_ok,
        "VM aborted feeding a parsed schedule object back into cron.next:\n{vm_err}"
    );
    assert!(tw_ok, "tree-walker run failed:\n{tw_err}");
    assert_eq!(
        vm_out, tw_out,
        "VM and tree-walker diverged on a parsed schedule object"
    );
    assert_eq!(vm_out.trim(), "1767269700000.0");
}

/// A malformed schedule-shaped literal (`__cron:"schedule"` but no bitmask
/// internals) is a Tier-2 error — and it must fail IDENTICALLY on both engines
/// (NOT a VM-only slab-borrow abort). We assert both engines fail the same way
/// and neither stderr carries the slab-borrow panic string.
#[test]
fn malformed_schedule_object_fails_identically_no_slab_panic() {
    let src = "import * as cron from \"std/cron\"\n\
        let r = cron.next({__cron: \"schedule\", foo: 1}, {after: 0, tzOffset: 0})\n\
        print(r[1] != nil)\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, tw_err) = run(src, true);
    // Same outcome on both engines (here: a Tier-2 error → non-success), and the
    // VM must NOT exhibit the slab-borrow panic.
    assert_eq!(vm_ok, tw_ok, "VM and tree-walker diverged on success");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on stdout");
    assert!(
        !vm_err.contains("slab-mode object"),
        "VM hit the slab-borrow panic — the bug is back:\n{vm_err}"
    );
    let _ = tw_err;
}
