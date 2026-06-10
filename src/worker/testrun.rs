//! DX D2 Task 5 — parallel test-FILE dispatch over the worker isolate substrate.
//!
//! `ascript test a.as b.as --parallel=N` runs each test FILE in its OWN shared-nothing
//! isolate (a fresh `Interp`/`Vm` on its own OS thread), so independent files execute in
//! parallel. The isolation boundary is the FILE — the coarse unit (the cost model from
//! the workers spec: ~0.5–2 ms isolate birth makes per-file the right granularity, not
//! per-test).
//!
//! ## The airlock shape (no new sendable kind)
//!
//! An isolate that finishes a file holds a [`crate::interp::TestSummary`] — a plain Rust
//! struct, NOT a `Value` and NOT sendable. The worker airlock crosses `Value` only (via
//! `serialize::check_sendable`/`encode`/`decode`). So the isolate encodes its summary as
//! a `Value::Object` (`TestSummary::to_value`) — `{passed, failed, failures:[{name,
//! message}]}`, all leaves being EXISTING sendable kinds — ships *those bytes* back, and
//! the parent decodes the object + reconstructs a `TestSummary` (`TestSummary::from_value`).
//! No new sendable `Value` kind is introduced.
//!
//! ## Determinism (§7)
//!
//! Files dispatch in parallel but the parent aggregates results in INPUT-FILE order (and,
//! within a file, the test-registration order the isolate preserved) before printing — so
//! the printed summary, the failure list order, and the exit code are byte-identical
//! regardless of which isolate finishes first. This module only RUNS one file in an
//! isolate and returns its summary; [`crate::run_tests_parallel`] owns the stable-order
//! aggregation.
//!
//! ## Nested workers
//!
//! A test file is a FULL top-level program (it has its own source), so it must behave
//! exactly like one run standalone — including its own `worker fn`/`worker class`/`worker
//! fn*` dispatch. We therefore force `IN_ISOLATE = false` for the file run (via
//! `isolate::with_isolate_flag(false, …)`, see `run_one_file`): the file's workers take
//! the NORMAL pool / code-slice path (recompile-from-source), NOT the inline-nesting path —
//! which would be wrong here, since inline-nesting assumes the entry is already a VM global
//! from an *enclosing* worker slice, and a test isolate has no such enclosing slice. No
//! deadlock (nested workers run on the per-isolate-thread pool), correct result.

use super::isolate;
use crate::interp::{Control, PackageMap, TestSummary};
use crate::stdlib::caps::CapSet;
use std::path::Path;

/// The outcome of running one test file in its isolate. `Ok` carries the reconstructed
/// summary; `Err` is a clean, human-readable reason the file could not be run (a load
/// error, an `exit()` during the run, a malformed isolate reply, or a dead isolate) —
/// never a panic. The parallel runner turns an `Err` into a synthetic failure for that
/// file so the OTHER files' results are still reported.
pub type FileRunResult = Result<TestSummary, String>;

/// Run the registered tests of ONE file `path` in a dedicated worker isolate and return
/// its [`TestSummary`] (reconstructed from the `Value::Object` the isolate ships back).
///
/// A fresh, shared-nothing `Interp`/`Vm` is built inside the isolate thread (the shared
/// `spawn_isolate` bootstrap); the file is loaded and its tests run THERE; the resulting
/// summary is encoded as a `Value::Object` and crosses back as `Send` bytes. The caller's
/// `!Send` runtime is never touched — only bytes (the file path, packages, caps in, the
/// encoded summary out) cross the boundary.
///
/// FALLIBLE-but-never-panicking: a file that fails to load, an `exit()` during the run, a
/// malformed reply, or a dead isolate all return `Err(reason)`.
pub async fn run_test_file_in_isolate(
    path: &Path,
    packages: Option<PackageMap>,
    caps: Option<CapSet>,
) -> FileRunResult {
    let path_str = path.to_string_lossy().into_owned();

    // `Send` back-channel for the one reply (single-shot dedicated isolate).
    let (reply_tx, reply_rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();

    // Clone caps/packages into the isolate closure, keeping the originals for the
    // spawn-failure inline fallback below (both are `Clone` + `Send`).
    let caps_iso = caps.clone();
    let packages_iso = packages.clone();
    let path_iso = path_str.clone();

    // Spawn the dedicated isolate. Everything captured is `Send` (String / HashMap of
    // PathBufs / CapSet / the reply sender).
    let handle = isolate::spawn_isolate(move |vm, mut rx| async move {
        let interp = vm.interp().clone();
        if let Some(caps) = caps_iso {
            interp.set_caps(caps);
        }
        if let Some(map) = packages_iso {
            interp.set_package_resolver(map);
        }

        // Wait for the start signal (the parent sends an empty message once it has
        // wired the bridge); if the handle is dropped first, the file is cancelled.
        if rx.recv().await.is_none() {
            return;
        }

        let result = run_one_file(&interp, &path_iso).await;
        let _ = reply_tx.send(result);
        // Loop ends when `rx` closes (handle dropped after the reply).
    });

    let handle = match handle {
        Ok(h) => h,
        // Could not spawn an isolate (thread/memory pressure): degrade to running the
        // file INLINE on the caller thread so the file is still reported (graceful
        // degradation, mirroring the pooled worker path). The caller's own LocalSet
        // drives it; the result is identical, only the parallelism is lost.
        Err(_) => {
            let interp = std::rc::Rc::new(crate::interp::Interp::new());
            if let Some(caps) = caps {
                interp.set_caps(caps);
            }
            if let Some(map) = packages {
                interp.set_package_resolver(map);
            }
            interp.install_self();
            return match run_one_file(&interp, &path_str).await {
                Ok(bytes) => decode_summary(&bytes, &path_str),
                Err(reason) => Err(reason),
            };
        }
    };

    // Send the start signal, then bridge the reply. Keep the handle alive across the
    // blocking wait so the isolate thread is not torn down before it replies.
    if handle.tx.send(Vec::new()).is_err() {
        return Err(format!(
            "test isolate for '{path_str}' terminated before it could start"
        ));
    }

    // Hold the handle alive (its Drop joins the thread) while we block for the reply on
    // a blocking helper, so the current-thread runtime is not stalled.
    //
    // CAVEAT (documented, not a hard cap): the 600 s bound only catches an isolate that
    // STOPS replying. It does NOT forcibly kill a *non-cooperative* isolate (a test file
    // spinning in `while (true)`): when the timeout fires we return an `Err`, but
    // `_handle` then drops → `IsolateHandle::Drop` does a `thread.join()` on the still-
    // running thread, which cannot be bounded, so `ascript test` would still block. This
    // is no worse than the serial runner (it also hangs on an infinite-loop test), and the
    // SP3 recursion-depth guard catches runaway *recursion*; a runaway *loop* is the
    // user's bug. The timeout exists to surface a wedged-but-not-spinning isolate cleanly,
    // not to enforce a wall-clock test budget.
    let _handle = handle;
    let reply = tokio::task::spawn_blocking(move || {
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(600))
            .ok()
    })
    .await
    .ok()
    .flatten();

    match reply {
        Some(Ok(bytes)) => decode_summary(&bytes, &path_str),
        Some(Err(reason)) => Err(reason),
        None => Err(format!(
            "test isolate for '{path_str}' terminated unexpectedly"
        )),
    }
}

/// Decode the airlock summary `Value::Object` bytes against a fresh caller-side interp and
/// reconstruct the [`TestSummary`]. A malformed shape / decode error is a clean `Err`
/// (never a panic), attributed to `path_str`.
fn decode_summary(bytes: &[u8], path_str: &str) -> FileRunResult {
    let interp = crate::interp::Interp::new();
    match crate::worker::serialize::decode(bytes, &interp) {
        Ok(v) => TestSummary::from_value(&v)
            .ok_or_else(|| format!("test isolate for '{path_str}' returned a malformed result")),
        Err(e) => Err(format!(
            "could not decode the test result for '{path_str}': {}",
            e.message()
        )),
    }
}

/// Load `path` into `interp` and run its registered tests, encoding the resulting
/// [`TestSummary`] as the airlock `Value::Object` bytes. Shared by the in-isolate path
/// and the spawn-failure inline fallback. Returns `Err(reason)` (never a panic) on a load
/// error or an `exit()` during the run.
async fn run_one_file(interp: &crate::interp::Interp, path: &str) -> Result<Vec<u8>, String> {
    // The hosting thread is a worker isolate (its `IN_ISOLATE` flag is TRUE), but the test
    // FILE must behave like a normal top-level program: a `worker fn` it dispatches should
    // take the full POOL path (recompile-from-source code slice), NOT the inline-nesting
    // path (which assumes the entry is already a VM global from an enclosing slice). So we
    // force the flag FALSE for the file run; the file's own workers then spawn their own
    // (per-thread) pool isolates — correct shared-nothing semantics, no deadlock.
    isolate::with_isolate_flag(false, || run_one_file_inner(interp, path)).await
}

async fn run_one_file_inner(
    interp: &crate::interp::Interp,
    path: &str,
) -> Result<Vec<u8>, String> {
    match interp.load_module(Path::new(path)).await {
        Ok(_) | Err(Control::Propagate(_)) => {}
        Err(Control::Panic(e)) => return Err(e.message),
        Err(Control::Exit(_)) => {
            return Err("exit() called during test run".to_string());
        }
    }
    let summary = match interp.run_registered_tests().await {
        Ok(summary) => summary,
        Err(Control::Exit(_)) => {
            return Err("exit() called during test run".to_string());
        }
        Err(Control::Panic(e)) => return Err(e.message),
        Err(Control::Propagate(_)) => TestSummary::default(),
    };
    let v = summary.to_value();
    crate::worker::serialize::encode(&v)
        .map(|(bytes, _shared)| bytes)
        .map_err(|e| format!("could not encode the test result: {}", e.message()))
}
