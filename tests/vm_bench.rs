//! VM-vs-tree-walker baseline benchmark harness (VM plan V11-T1).
//!
//! This is the perf-gate substrate. It times the GENERIC (un-specialized) bytecode
//! VM (`vm_run_source`) against the tree-walking interpreter (`run_source_exit`) on
//! a set of COMPUTE-BOUND, import-free AScript programs, and prints a table of
//! per-benchmark wall-clock plus the VM/tree-walker speedup ratio.
//!
//! It is an `#[ignore]`d test so it never runs in the normal suite (it would just
//! burn time and produce non-deterministic timings). Run it on demand, in RELEASE,
//! to get meaningful numbers:
//!
//! ```text
//! cargo test --release --test vm_bench -- --ignored --nocapture
//! ```
//!
//! The benchmark programs deliberately use ONLY features the VM compiler supports
//! pre-V12 (no `import`): recursion, loops, arithmetic, property/method access on
//! local objects/classes, and string building via `+`/templates. Each engine runs
//! the same source; both `run_source_exit` and `vm_run_source` build their own
//! `tokio::task::LocalSet` internally, so we just await them from a current-thread
//! runtime.
//!
//! Methodology: each program is run once to warm up (instruction cache, allocator),
//! then `RUNS` timed runs are taken and the MEDIAN and MIN reported. The speedup is
//! `tree-walker median / VM median` — a value `>= 2.0` means the VM is at least 2×
//! faster on that benchmark.
//!
//! V11-T6 (PERFORMANCE GATE): this harness now times THREE engines — the tree-walker,
//! the GENERIC (un-specialized) VM (`vm_run_source_generic`), and the SPECIALIZED VM
//! (`vm_run_source`, the default since V11-T2..T5: shapes + polymorphic ICs on
//! GET_PROP/SET_PROP/CALL_METHOD + PEP-659 adaptive arithmetic/globals). It prints,
//! per benchmark: tree-walker / generic-VM / specialized-VM medians, the
//! specialized-vs-tree-walker speedup (the GATE metric), and the
//! specialized-vs-generic speedup (the SPECIALIZATION win + no-regression check).
//!
//! GATE (spec): ">= 2× on COMPUTE-BOUND, NO regression on ANY benchmark."
//!   - Compute-bound = fib, sum recursion, numeric loop, while loop, property r/w,
//!     method dispatch. Specialized-vs-tw MUST be >= 2.0×.
//!   - No-regression = specialized-vs-generic MUST be >= ~1.0× on EVERY benchmark
//!     (the IC/adaptive layer must not slow anything down vs generic; small noise OK).
//!   - String concat / template build are ALLOCATION-bound (both engines build the
//!     same shared `Rc<str>` strings and pay the same allocator cost), so they are
//!     EXEMPT from the >= 2× compute-bound target. They must still show NO REGRESSION
//!     (specialized >= generic, and ideally >= tree-walker).
//!
//! ───────────────────────────────────────────────────────────────────────────────
//! GATE RESULT — PASS (recorded V11-T6, release; numbers are machine-dependent,
//! re-run to refresh). Both gate conditions held:
//!
//!   benchmark                  kind     spec/tw   spec/gen
//!   fib(30) recursion          compute    4.89x     1.03x
//!   sum recursion (500 x2000)  compute    5.83x     1.05x
//!   numeric loop (1e6)         compute    2.84x     1.02x
//!   while loop (1e6)           compute    4.61x     1.03x
//!   property r/w (1e6)         compute    3.08x     1.62x
//!   method dispatch (1e6)      compute    2.97x     2.03x
//!   string concat (50000)      alloc      1.28x     1.05x   (EXEMPT from >= 2x)
//!   template build (50000)     alloc      1.20x     1.01x   (EXEMPT from >= 2x)
//!   geomean spec/tw = 2.92x
//!
//!   (a) COMPUTE-BOUND >= 2× spec/tw: PASS — all six (min 2.84×, method dispatch
//!       2.97×). The generic baseline (V11-T1) had method dispatch at 1.79× and it
//!       was below 2× even after the IC landed (~1.9×); V11-T6 added an in-place
//!       frame-push fast path for the `CALL_METHOD` IC hit (no fresh Fiber + no
//!       recursive `Vm::run`, mirroring the `Op::Call` VM-closure arm), pushing it
//!       to ~2.0× spec/gen and ~3× spec/tw.
//!   (b) NO REGRESSION (spec/gen >= ~1.0×): PASS — every benchmark, incl. the
//!       allocation-bound string benches. The pre-T6 specialized VM showed a small
//!       (~3-4%) regression vs generic on the pure-arithmetic benches (fib/sum/
//!       numeric loop landed at 0.96-0.97× spec/gen) because the offset-keyed
//!       adaptive side maps were `HashMap<usize,_>` with SipHash, hashed on EVERY
//!       arithmetic op. V11-T6 switched those side maps to a pass-through
//!       `OffsetHasher` (`chunk::OffsetMap`), erasing that overhead (now 1.02-1.05×).
//!   (c) STRING-BUILDING EXEMPTION: `string concat` / `template build` build the
//!       same shared `Rc<str>` strings on both engines and are dominated by the
//!       allocator, not interpreter dispatch — not compute-bound, so exempt from the
//!       >= 2× target. They still pass the no-regression check (spec/gen >= 1.0×)
//!       and even beat the tree-walker (1.2-1.3× spec/tw).
//! ───────────────────────────────────────────────────────────────────────────────

use std::time::{Duration, Instant};

/// Number of timed runs per (program, engine). The median and min are reported.
const RUNS: usize = 7;

/// Which engine to time.
#[derive(Clone, Copy)]
enum Engine {
    TreeWalker,
    /// The GENERIC (un-specialized) VM — all ICs / PEP-659 adaptive sites disabled.
    GenericVm,
    /// The SPECIALIZED VM (default): shapes + polymorphic ICs + adaptive arith/globals.
    SpecializedVm,
}

/// Run `src` once on `engine`, asserting it succeeds. Returns the elapsed time.
async fn time_once(engine: Engine, src: &str, name: &str) -> Duration {
    let start = Instant::now();
    match engine {
        Engine::TreeWalker => {
            ascript::run_source_exit(src)
                .await
                .unwrap_or_else(|e| panic!("tree-walker failed on `{name}`: {e}"));
        }
        Engine::GenericVm => {
            ascript::vm_run_source_generic(src)
                .await
                .unwrap_or_else(|e| panic!("generic VM failed on `{name}`: {e}"));
        }
        Engine::SpecializedVm => {
            ascript::vm_run_source(src)
                .await
                .unwrap_or_else(|e| panic!("specialized VM failed on `{name}`: {e}"));
        }
    }
    start.elapsed()
}

/// Warm up once, then take `RUNS` timed runs of `src` on `engine`.
/// Returns `(median, min)`.
async fn measure(engine: Engine, src: &str, name: &str) -> (Duration, Duration) {
    // Warm-up (not timed).
    let _ = time_once(engine, src, name).await;

    let mut samples: Vec<Duration> = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        samples.push(time_once(engine, src, name).await);
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    let min = samples[0];
    (median, min)
}

/// Format a Duration as a right-aligned millisecond string.
fn ms(d: Duration) -> String {
    format!("{:>10.3}", d.as_secs_f64() * 1000.0)
}

/// A single benchmark program.
struct Bench {
    name: &'static str,
    src: &'static str,
    /// `true` = COMPUTE-bound (subject to the >= 2× specialized-vs-tw gate);
    /// `false` = ALLOCATION-bound (string building — exempt from >= 2×, but must
    /// show no regression).
    compute_bound: bool,
}

/// The compute-bound, import-free benchmark corpus.
fn benches() -> Vec<Bench> {
    vec![
        // ── deep / heavy recursion ──────────────────────────────────────────
        Bench {
            name: "fib(30) recursion",
            compute_bound: true,
            src: r#"
fn fib(n) {
  if (n < 2) { return n }
  return fib(n - 1) + fib(n - 2)
}
print(fib(30))
"#,
        },
        Bench {
            compute_bound: true,
            // Shallow-depth (500) linear recursion called 2000 times: exercises
            // call/return overhead heavily without relying on deep native-stack
            // recursion (a documented VM architectural non-goal — spec §7).
            name: "sum recursion (500 x2000)",
            src: r#"
fn sumto(n) {
  if (n == 0) { return 0 }
  return n + sumto(n - 1)
}
let total = 0
for (i in 0..2000) { total = total + sumto(500) }
print(total)
"#,
        },
        // ── tight numeric loop ──────────────────────────────────────────────
        Bench {
            name: "numeric loop (1e6)",
            compute_bound: true,
            src: r#"
let sum = 0
for (i in 0..1000000) { sum = sum + i }
print(sum)
"#,
        },
        Bench {
            name: "while loop (1e6)",
            compute_bound: true,
            src: r#"
let i = 0
let sum = 0
while (i < 1000000) {
  sum = sum + i
  i = i + 1
}
print(sum)
"#,
        },
        // ── property access (read + write object fields) ─────────────────────
        Bench {
            name: "property r/w (1e6)",
            compute_bound: true,
            src: r#"
let o = { x: 0, y: 1 }
for (i in 0..1000000) {
  o.x = o.x + o.y
}
print(o.x)
"#,
        },
        // ── method dispatch on a class instance ─────────────────────────────
        Bench {
            name: "method dispatch (1e6)",
            compute_bound: true,
            src: r#"
class Counter {
  fn init() { self.n = 0 }
  fn bump() { self.n = self.n + 1 }
}
let c = Counter()
for (i in 0..1000000) { c.bump() }
print(c.n)
"#,
        },
        // ── string building via + and templates ─────────────────────────────
        Bench {
            name: "string concat (50000)",
            compute_bound: false,
            src: r#"
let s = ""
for (i in 0..50000) { s = s + "x" }
print(len(s))
"#,
        },
        Bench {
            name: "template build (50000)",
            compute_bound: false,
            src: r#"
let s = ""
for (i in 0..50000) { s = `${s}y` }
print(len(s))
"#,
        },
    ]
}

/// A generous worker-thread stack so the recursion benchmarks (and the
/// `!Send`, current-thread tokio runtime they drive) have room — the default
/// test-thread stack (2 MiB) is too small for fib-style recursion under the
/// async tree-walker.
const WORKER_STACK_BYTES: usize = 256 * 1024 * 1024;

#[test]
#[ignore = "perf harness — run explicitly: cargo test --release --test vm_bench -- --ignored --nocapture"]
fn vm_vs_treewalker_baseline() {
    // Run the whole harness on a dedicated big-stack thread with its own
    // current-thread tokio runtime (the interpreter is `!Send`).
    std::thread::Builder::new()
        .name("vm-bench".into())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");
            rt.block_on(run_baseline());
        })
        .expect("spawn bench thread")
        .join()
        .expect("bench thread panicked");
}

async fn run_baseline() {
    let benches = benches();

    println!();
    println!("VM PERF GATE (V11-T6): tree-walker vs GENERIC VM vs SPECIALIZED VM");
    println!(
        "runs/bench = {RUNS} (warm-up + median); profile = {}",
        if cfg!(debug_assertions) {
            "DEBUG (run with --release for real numbers!)"
        } else {
            "release"
        }
    );
    println!();
    println!(
        "{:<28} {:>9} {:>11} {:>11} {:>11} {:>11} {:>11}",
        "benchmark", "kind", "tw (ms)", "gen (ms)", "spec (ms)", "spec/tw", "spec/gen",
    );
    println!("{}", "-".repeat(28 + 9 + 11 * 5 + 6));

    // Collected for the gate verdict.
    let mut tw_ratios: Vec<f64> = Vec::new();
    let mut compute_misses: Vec<(&str, f64)> = Vec::new();
    let mut regressions: Vec<(&str, f64)> = Vec::new();

    for b in &benches {
        let (tw_med, _tw_min) = measure(Engine::TreeWalker, b.src, b.name).await;
        let (gen_med, _gen_min) = measure(Engine::GenericVm, b.src, b.name).await;
        let (spec_med, _spec_min) = measure(Engine::SpecializedVm, b.src, b.name).await;

        let spec_vs_tw = tw_med.as_secs_f64() / spec_med.as_secs_f64();
        let spec_vs_gen = gen_med.as_secs_f64() / spec_med.as_secs_f64();
        tw_ratios.push(spec_vs_tw);

        // No-regression: specialized must be >= generic. Allow 3% timing noise.
        const NOISE: f64 = 0.97;
        if spec_vs_gen < NOISE {
            regressions.push((b.name, spec_vs_gen));
        }
        // Compute-bound gate: specialized-vs-tw >= 2.0×.
        if b.compute_bound && spec_vs_tw < 2.0 {
            compute_misses.push((b.name, spec_vs_tw));
        }

        println!(
            "{:<28} {:>9} {} {} {} {:>10.2}x {:>10.2}x",
            b.name,
            if b.compute_bound { "compute" } else { "alloc" },
            ms(tw_med),
            ms(gen_med),
            ms(spec_med),
            spec_vs_tw,
            spec_vs_gen,
        );
    }

    println!();
    let geomean = tw_ratios.iter().map(|r| r.ln()).sum::<f64>() / tw_ratios.len() as f64;
    let geomean = geomean.exp();
    let n_at_2x = tw_ratios.iter().filter(|&&r| r >= 2.0).count();
    println!(
        "geomean spec/tw speedup = {geomean:.2}x   ({n_at_2x}/{} benches at >= 2.0x)",
        tw_ratios.len()
    );
    println!();

    // ── Gate verdict ────────────────────────────────────────────────────────
    println!("GATE: '>= 2x on COMPUTE-BOUND, NO regression on ANY benchmark'");
    if compute_misses.is_empty() {
        println!("  [PASS] every COMPUTE-bound benchmark is >= 2.0x the tree-walker");
    } else {
        println!("  [FAIL] COMPUTE-bound benchmark(s) below 2.0x specialized-vs-tw:");
        for (name, r) in &compute_misses {
            println!("           - {name}: {r:.2}x");
        }
    }
    if regressions.is_empty() {
        println!("  [PASS] no regression: specialized >= generic on every benchmark");
    } else {
        println!("  [FAIL] specialized SLOWER than generic (real regression):");
        for (name, r) in &regressions {
            println!("           - {name}: spec/gen {r:.2}x");
        }
    }
    println!("  [NOTE] string concat / template build are ALLOCATION-bound (shared Rc<str>);");
    println!("         EXEMPT from the >= 2x compute-bound target, but checked for no-regression.");
    println!();
}
