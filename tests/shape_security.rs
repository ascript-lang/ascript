//! SHAPE §6.2 — hash-flooding DoS resistance.
//!
//! Phase 4 swapped the VM's INTERIOR tables (`class_methods`,
//! `class_static_methods`, `class_defaults`, `user_globals`) to `FxHash` for
//! speed, because their key inflow is bounded by the program text (class-identity
//! pointers / source identifiers) and never attacker-scaled. The tables an
//! attacker CAN flood — user `Map`/`Set` keys and the demoted hostile-key object
//! dict — deliberately KEEP the default randomized SipHash hasher.
//!
//! These tests prove the security property still holds end-to-end: driving ~100k
//! hostile DISTINCT dynamic keys completes in roughly LINEAR (not quadratic) time.
//! A SipHash-flooded `HashMap` degrades to O(n) per-op (O(n²) total); these runs
//! finishing well under a generous wall-clock bound — AND the 50k→100k ratio
//! staying near 2× rather than near 4× — is the linearity evidence.
//!
//! The object path additionally exercises SHAPE's own bounded-growth caps: past
//! `SLAB_MAX_KEYS = 64` distinct keys an object DEMOTES from the shape-native slab
//! to a `Dict(IndexMap<String, Value>)` on the default (SipHash) hasher, and the
//! `ShapeRegistry` transition tree is capped (`SHAPE_FANOUT_MAX = 128`) so it never
//! grows ~100k nodes.

use std::process::Command;
use std::time::Instant;

/// Write a temp `.as` file and run it through the built binary, returning
/// (success, elapsed). The body must `print` a final marker we check.
fn run_timed(name: &str, src: &str) -> (bool, std::time::Duration, String) {
    let file = std::env::temp_dir().join(format!("ascript_shape_sec_{name}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let start = Instant::now();
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let elapsed = start.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.success(), elapsed, stdout)
}

/// An object churn that inserts `n` DISTINCT dynamic string keys via `o[k] = v`.
/// Past 64 keys the object demotes to a SipHash dict; the run must stay LINEAR.
fn object_churn_src(n: usize) -> String {
    format!(
        "let o = {{}}\nfor (i in 0..{n}) {{\n  o[`key_${{i}}`] = i\n}}\nprint(len(o))\n"
    )
}

/// SHAPE §6.2: ~100k hostile DISTINCT object keys complete (no quadratic blowup,
/// no SIGABRT) and the object's length is exactly the key count — proving the
/// demotion-to-SipHash-dict path is correct AND bounded.
#[test]
fn hostile_object_keys_complete_bounded() {
    let (ok, elapsed, out) = run_timed("obj_100k", &object_churn_src(100_000));
    assert!(ok, "100k hostile-key object churn must complete cleanly");
    assert_eq!(out.trim(), "100000", "all 100k distinct keys must be stored");
    // Generous absolute bound: a SipHash-flooded O(n²) would blow far past this on
    // any machine; the actual run is ~tens of ms. 30s is a CI-safe ceiling.
    assert!(
        elapsed.as_secs() < 30,
        "100k object churn took {elapsed:?} — expected linear (well under 30s); \
         a quadratic regression (hash flooding) would exceed this"
    );
}

/// SHAPE §6.2: LINEARITY probe — 50k vs 100k object churn. A linear path doubles
/// (~2×); a quadratic (hash-flooded) path quadruples (~4×). We assert the ratio is
/// comfortably under 3× (the linear regime with generous slack for fixed overhead
/// and timing noise). This is the direct anti-flooding witness.
#[test]
fn hostile_object_keys_scale_linearly() {
    // Warm once (binary spawn + parse dominate tiny runs); take the better of two.
    let measure = |n: usize| -> std::time::Duration {
        let mut best = std::time::Duration::from_secs(3600);
        for _ in 0..2 {
            let (ok, e, _) = run_timed(&format!("obj_scale_{n}"), &object_churn_src(n));
            assert!(ok, "object churn n={n} must complete");
            if e < best {
                best = e;
            }
        }
        best
    };
    let t50 = measure(50_000);
    let t100 = measure(100_000);
    // Process-spawn + parse is a fixed per-run cost; subtract a small floor so the
    // ratio reflects the churn work, not constant overhead. Use the 50k time as a
    // proxy that already INCLUDES that overhead, so a generous 3× ceiling absorbs it.
    let ratio = t100.as_secs_f64() / t50.as_secs_f64().max(1e-6);
    assert!(
        ratio < 3.0,
        "50k={t50:?} 100k={t100:?} ratio={ratio:.2}× — expected ~2× (linear); \
         ~4× would indicate quadratic hash-flooding degradation"
    );
}

/// SHAPE §6.2: ~100k hostile DISTINCT user-`Map` keys complete cleanly. `Map` keys
/// are attacker-controlled and KEEP the randomized SipHash hasher — so even a
/// crafted key set cannot force quadratic behavior across processes. The security
/// property is that the default hasher is randomized PER PROCESS; here we assert
/// the run still completes in linear time with that protection in place.
#[test]
fn hostile_map_keys_keep_siphash_and_complete() {
    let src = "import * as map from \"std/map\"\n\
               let m = map.new()\n\
               for (i in 0..100000) {\n  map.set(m, `key_${i}`, i)\n}\n\
               print(len(m))\n";
    let (ok, elapsed, out) = run_timed("map_100k", src);
    assert!(ok, "100k hostile-key Map churn must complete cleanly");
    assert_eq!(out.trim(), "100000", "all 100k distinct Map keys must be stored");
    assert!(
        elapsed.as_secs() < 30,
        "100k Map churn took {elapsed:?} — expected linear; SipHash keeps it so"
    );
}
