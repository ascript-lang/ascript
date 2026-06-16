//! REGION Phase-0 probe (spec §5.2): per-allocation birth/death accounting keyed
//! by task identity. Dev-only (`--features region-probe`); compiled OUT otherwise.
//!
//! The instrument measures the **region-eligible share** — allocations born at a
//! bytecode *literal* site (`Op::NewObject`/`Op::NewArray`) whose lifetime provably
//! ends *within their birth task*, split by container kind and site class. Task 0.3
//! consumes the dumped histogram against the §5.3 GO/NO-GO checkpoint.
//!
//! ## Why it must not distort what it measures
//!
//! Adding a `Drop` impl to a container cell can change drop-order/timing and forbid
//! niche layout optimizations. Therefore EVERY part of this seam — the per-cell
//! `probe` field, the `Drop` impls, the `birth`/`death` calls, and the task
//! brackets — is `#[cfg(feature = "region-probe")]`. A default build has none of it
//! (byte-identical to pre-REGION; the four-mode differential proves it). The probe
//! must pass the FULL default test suite identically under the feature.
//!
//! ## VM-only by design
//!
//! Region activation (Phase 1+) is VM-only (specialized lane), so the probe measures
//! the VM. The VM literal handlers (`Op::NewObject`/`Op::NewArray`) classify their
//! freshly-built cell as [`SiteClass::Literal`]; every other construction path —
//! including ALL tree-walker literal sites — defaults to [`SiteClass::Native`]. A
//! tree-walker run therefore reports everything as `Native`, which is correct: the
//! tree-walker is a permanent plain-allocator oracle and never recycles.
//!
//! ## Task identity
//!
//! `CURRENT_TASK` is the id of the task currently executing on this thread (0 = the
//! main/top-level "task", which never retires before program end). [`enter_task`]
//! installs a fresh id at each `spawn_local` body and the `http.serve` per-request
//! handler; its [`TaskGuard`] restores the parent id and **retires** the id on drop
//! (including cancel-on-drop — an aborted task runs destructors). A cell whose birth
//! task is no longer live at death "escaped its task".

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

/// The birth-site class of a container cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SiteClass {
    /// Built by a VM bytecode literal handler (`Op::NewObject`/`Op::NewArray`).
    Literal,
    /// Built by any other path (stdlib, tree-walker, deserialization, `.from`, …).
    Native,
}

/// Which container kind a death belongs to (selects the [`ProbeStats`] row).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeKind {
    Object,
    Array,
    Map,
    Set,
    Instance,
}

/// A cell's birth record: the task that constructed it + its site class.
#[derive(Clone, Copy, Debug)]
pub struct Birth {
    pub task: u64,
    pub site: SiteClass,
}

impl Default for Birth {
    /// Cells default to `Native` born in `CURRENT_TASK`. Constructors call this so a
    /// cell built outside a VM literal handler is correctly attributed to its task.
    fn default() -> Self {
        Birth {
            task: CURRENT_TASK.with(|c| c.get()),
            site: SiteClass::Native,
        }
    }
}

/// `[site_class][died_in_birth_task]` counts, per container kind.
/// Index `[SiteClass as usize][in_task as usize]`: `[0]` = `Literal`/`Native` per the
/// enum order, `[..][0]` = escaped, `[..][1]` = died-in-task.
#[derive(Default, Clone, Copy, Debug)]
pub struct ProbeStats {
    pub object: [[u64; 2]; 2],
    pub array: [[u64; 2]; 2],
    pub map: [[u64; 2]; 2],
    pub set: [[u64; 2]; 2],
    pub instance: [[u64; 2]; 2],
}

thread_local! {
    /// The task currently executing on this thread. 0 = main/top-level.
    static CURRENT_TASK: Cell<u64> = const { Cell::new(0) };
    /// Monotonic source of fresh task ids (0 is reserved for main).
    static NEXT_TASK: Cell<u64> = const { Cell::new(1) };
    /// The set of task ids still live (entered, not yet retired).
    static LIVE_TASKS: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    /// The accumulating histogram, dumped at program end.
    static STATS: RefCell<ProbeStats> = RefCell::new(ProbeStats::default());
}

/// RAII task bracket. Installed at every `spawn_local` body and the `http.serve`
/// per-request handler; restores the parent task id and **retires** this id on drop
/// (so a drop AFTER retirement is classified "escaped its task"). The guard is a
/// plain owned value — no `RefCell` borrow is held across `.await`.
pub struct TaskGuard {
    prev: u64,
    id: u64,
}

/// Enter a fresh task: mint an id, mark it live, make it current. Returns the guard
/// that retires it on drop.
pub fn enter_task() -> TaskGuard {
    let id = NEXT_TASK.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    });
    LIVE_TASKS.with(|l| {
        l.borrow_mut().insert(id);
    });
    let prev = CURRENT_TASK.with(|c| c.replace(id));
    TaskGuard { prev, id }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        CURRENT_TASK.with(|c| c.set(self.prev));
        LIVE_TASKS.with(|l| {
            l.borrow_mut().remove(&self.id);
        });
    }
}

/// Record a cell's birth at `site`, attributing it to `CURRENT_TASK`.
pub fn birth(site: SiteClass) -> Birth {
    Birth {
        task: CURRENT_TASK.with(|c| c.get()),
        site,
    }
}

/// Record a cell's death. `kind` selects the [`ProbeStats`] row; `in-task` ⇔ the
/// birth task is task 0 (main, never retires before program end) OR is still live.
pub fn death(kind: ProbeKind, b: Birth) {
    let in_task =
        b.task == 0 || LIVE_TASKS.with(|l| l.borrow().contains(&b.task));
    let site = b.site as usize;
    let live = in_task as usize;
    STATS.with(|s| {
        let mut s = s.borrow_mut();
        let row = match kind {
            ProbeKind::Object => &mut s.object,
            ProbeKind::Array => &mut s.array,
            ProbeKind::Map => &mut s.map,
            ProbeKind::Set => &mut s.set,
            ProbeKind::Instance => &mut s.instance,
        };
        row[site][live] = row[site][live].saturating_add(1);
    });
}

/// Snapshot the current histogram (test inspection).
pub fn stats() -> ProbeStats {
    STATS.with(|s| *s.borrow())
}

/// Reset the histogram + task bookkeeping (test isolation).
pub fn reset() {
    STATS.with(|s| *s.borrow_mut() = ProbeStats::default());
    LIVE_TASKS.with(|l| l.borrow_mut().clear());
    CURRENT_TASK.with(|c| c.set(0));
    NEXT_TASK.with(|n| n.set(1));
}

/// Dump the histogram to `$ASCRIPT_REGION_PROBE_OUT` (a single JSON line) at program
/// end. Absent env var → no output. NEVER panics: an IO error is logged to stderr and
/// swallowed (the probe must not change the exit behavior of the program it measures).
pub fn dump() {
    let Ok(path) = std::env::var("ASCRIPT_REGION_PROBE_OUT") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let s = stats();
    // Hand-rolled JSON (no serde dependency for a dev instrument). Layout per kind:
    //   "object": { "literal": {"in_task": N, "escaped": N},
    //               "native":  {"in_task": N, "escaped": N} }
    // Index reminder: row[SiteClass as usize][in_task as usize].
    let kind_json = |row: &[[u64; 2]; 2]| -> String {
        format!(
            "{{\"literal\":{{\"in_task\":{},\"escaped\":{}}},\"native\":{{\"in_task\":{},\"escaped\":{}}}}}",
            row[SiteClass::Literal as usize][1],
            row[SiteClass::Literal as usize][0],
            row[SiteClass::Native as usize][1],
            row[SiteClass::Native as usize][0],
        )
    };
    let line = format!(
        "{{\"object\":{},\"array\":{},\"map\":{},\"set\":{},\"instance\":{}}}\n",
        kind_json(&s.object),
        kind_json(&s.array),
        kind_json(&s.map),
        kind_json(&s.set),
        kind_json(&s.instance),
    );
    use std::io::Write;
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = appended {
        eprintln!("region-probe: failed to write {path}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The probe's thread-locals are shared per test thread; `reset()` at the top of
    // each test isolates them (cargo runs tests on multiple threads, but each test
    // body runs start-to-finish on one thread, so the thread-locals it touches are
    // its own — `reset()` clears any residue from a prior test reused on this thread).

    #[test]
    fn literal_dies_in_task_is_in_task() {
        reset();
        // Enter a fresh task, record a Literal birth in it, then record its death
        // WHILE the task is still live → [Literal][in_task].
        let guard = enter_task();
        let b = birth(SiteClass::Literal);
        assert_eq!(b.site, SiteClass::Literal);
        assert_ne!(b.task, 0, "a spawned task gets a non-zero id");
        death(ProbeKind::Object, b);
        let s = stats();
        assert_eq!(s.object[SiteClass::Literal as usize][1], 1, "in-task");
        assert_eq!(s.object[SiteClass::Literal as usize][0], 0, "not escaped");
        drop(guard);
    }

    #[test]
    fn literal_outliving_its_task_is_escaped() {
        reset();
        // Birth a Literal cell inside a task, retire the task (drop the guard), THEN
        // record its death → the birth task is no longer live → [Literal][escaped].
        let b = {
            let _guard = enter_task();
            birth(SiteClass::Literal)
        }; // guard dropped here — task retired, but the Birth record escaped.
        death(ProbeKind::Object, b);
        let s = stats();
        assert_eq!(s.object[SiteClass::Literal as usize][0], 1, "escaped");
        assert_eq!(s.object[SiteClass::Literal as usize][1], 0, "not in-task");
    }

    #[test]
    fn main_task_births_never_escape() {
        reset();
        // A birth in task 0 (main) is always in-task — main never retires before end.
        let b = birth(SiteClass::Native);
        assert_eq!(b.task, 0);
        death(ProbeKind::Array, b);
        let s = stats();
        assert_eq!(s.array[SiteClass::Native as usize][1], 1, "main is in-task");
        assert_eq!(s.array[SiteClass::Native as usize][0], 0);
    }

    #[test]
    fn task_guard_restores_parent_and_retires_id() {
        reset();
        assert_eq!(CURRENT_TASK.with(|c| c.get()), 0);
        let g1 = enter_task();
        let id1 = CURRENT_TASK.with(|c| c.get());
        assert_ne!(id1, 0);
        assert!(LIVE_TASKS.with(|l| l.borrow().contains(&id1)));
        // Nested task: parent is restored on inner drop.
        {
            let _g2 = enter_task();
            let id2 = CURRENT_TASK.with(|c| c.get());
            assert_ne!(id2, id1);
        }
        assert_eq!(CURRENT_TASK.with(|c| c.get()), id1, "parent restored");
        drop(g1);
        // The outer guard's Drop retires id1 (the cancel-on-drop / structured-exit
        // path — abort runs destructors, so a cancelled task retires here too).
        assert!(!LIVE_TASKS.with(|l| l.borrow().contains(&id1)), "id retired");
        assert_eq!(CURRENT_TASK.with(|c| c.get()), 0, "back to main");
    }

    #[test]
    fn cancel_on_drop_retires_the_task() {
        reset();
        // Model cancel-on-drop: a spawned task's guard is created, the task is then
        // aborted (its future dropped) WITHOUT running to the end — the guard's Drop
        // still fires (abort runs destructors), retiring the id. A subsequent death
        // of a cell born in that task is therefore classified escaped.
        let b = {
            let _g = enter_task(); // simulates the spawned body's RAII bracket
            birth(SiteClass::Literal)
            // `_g` drops here exactly as it would when the body's future is aborted.
        };
        // After cancellation, the birth task must be retired.
        assert!(!LIVE_TASKS.with(|l| l.borrow().contains(&b.task)));
        death(ProbeKind::Object, b);
        assert_eq!(stats().object[SiteClass::Literal as usize][0], 1, "escaped");
    }

    #[test]
    fn dump_never_panics_on_missing_env() {
        reset();
        // No env set → no output, no panic.
        std::env::remove_var("ASCRIPT_REGION_PROBE_OUT");
        dump();
    }
}
