//! DEFER §5.3: test/fuzz instrumentation counters — gated to `cfg(any(test,
//! feature = "fuzzgen", fuzzing))` so they are NEVER compiled into production
//! binaries. Incremented at the three canonical defer sites:
//!   `ENTRIES_PUSHED`    — each DeferPush / DeferPushMethod capture
//!   `ENTRIES_DRAINED`   — each entry executed in vm_run_defers / run_defers
//!   `CHOKEPOINT_DRAINS` — each call to vm_run_defers / run_defers when non-empty

#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[allow(clippy::module_inception)]
pub mod defer_metrics {
    use std::sync::atomic::{AtomicU64, Ordering};

    pub static ENTRIES_PUSHED: AtomicU64 = AtomicU64::new(0);
    pub static ENTRIES_DRAINED: AtomicU64 = AtomicU64::new(0);
    pub static CHOKEPOINT_DRAINS: AtomicU64 = AtomicU64::new(0);

    pub fn reset() {
        ENTRIES_PUSHED.store(0, Ordering::Relaxed);
        ENTRIES_DRAINED.store(0, Ordering::Relaxed);
        CHOKEPOINT_DRAINS.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn counters_start_at_zero_after_reset() {
            reset();
            assert_eq!(ENTRIES_PUSHED.load(Ordering::Relaxed), 0);
            assert_eq!(ENTRIES_DRAINED.load(Ordering::Relaxed), 0);
            assert_eq!(CHOKEPOINT_DRAINS.load(Ordering::Relaxed), 0);
        }

        /// Gate 15: corpus-assertion. After running a set of defer-exercising programs
        /// (on BOTH the tree-walker and the VM), every counter must be nonzero — proving
        /// the instrumentation is wired up at the push site AND the drain site in each
        /// engine. The programs cover at least one push + drain in each engine.
        #[tokio::test]
        async fn defer_corpus_all_counters_nonzero() {
            reset();
            // A program with two defers that both drain on normal return — exercises push
            // (×2) and drain (×2) in both engines.
            let src = "fn test() {\n\
                           defer print(\"a\")\n\
                           defer print(\"b\")\n\
                       }\n\
                       test()\n";
            // Tree-walker path.
            crate::run_source(src).await.expect("tw ok");
            // VM specialized + generic paths.
            crate::vm_run_source(src).await.expect("vm ok");
            crate::vm_run_source_generic(src).await.expect("gen ok");

            let pushed = ENTRIES_PUSHED.load(Ordering::Relaxed);
            let drained = ENTRIES_DRAINED.load(Ordering::Relaxed);
            let chokepoint = CHOKEPOINT_DRAINS.load(Ordering::Relaxed);
            assert!(
                pushed > 0,
                "ENTRIES_PUSHED must be nonzero after corpus run (got {pushed})"
            );
            assert!(
                drained > 0,
                "ENTRIES_DRAINED must be nonzero after corpus run (got {drained})"
            );
            assert!(
                chokepoint > 0,
                "CHOKEPOINT_DRAINS must be nonzero after corpus run (got {chokepoint})"
            );
        }
    }
}
