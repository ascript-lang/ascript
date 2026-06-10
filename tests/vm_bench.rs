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
//! GATE RESULT — PASS (recorded SP8 Phase A, 2026-06-04, release; same machine as the
//! SP8 A0 baseline). The SP8 index-stable global-access cache (`GlobalCache::IndexBound`
//! guarded by `struct_gen`, which bumps only on DEFINE, never on a reassigning
//! SET_GLOBAL) recovers the user-global regression. Both gate conditions held.
//!
//!   benchmark                  kind     spec/tw   spec/gen   (A0 baseline spec/tw)
//!   fib(30) recursion          compute    4.88x     1.03x     (4.88x)
//!   sum recursion (500 x2000)  compute    5.83x     1.11x     (5.34x → recovered)
//!   numeric loop (1e6)         compute    2.82x     1.21x     (2.96x; index-dominated)
//!   while loop (1e6)           compute    4.43x     1.49x     (3.23x → recovered)
//!   property r/w (1e6)         compute    2.96x     1.56x     (3.09x)
//!   method dispatch (1e6)      compute    2.85x     1.79x     (2.69x)
//!   string concat (50000)      alloc      1.25x     1.00x     (EXEMPT; allocator noise)
//!   template build (50000)     alloc      1.16x     0.99x     (EXEMPT)
//!   geomean spec/tw = 2.82x   (A0 baseline 2.73x → 2.82x)
//!
//!   The regression-target globals recovered most: `while loop` 3.23x → 4.43x (both
//!   `i` and `sum` are reassigned globals read+written twice/iter — the index cache
//!   hits every iteration), `sum recursion` 5.34x → 5.83x (back to the V11-T6 figure).
//!   `numeric loop`/`property r/w` are dominated by the frame-LOCAL for-range index,
//!   not the global, so they moved little (within run-to-run noise). NO spec-vs-generic
//!   regression (the alloc benches hover at ~1.0x spec/gen with allocator jitter).
//! ───────────────────────────────────────────────────────────────────────────────
//! GATE RESULT — PASS (recorded SP8 final / Phase B+C, 2026-06-04, release; same
//! machine). Phase B added #136 capture-by-value upvalues (never-reassigned captures
//! copied into a fresh private cell at Op::Closure; the declaring frame keeps a plain
//! stack local — no cell alloc, no per-access RefCell borrow) + the `closure capture`
//! bench. Both gate conditions held; geomean recovered toward the V11-T6 2.92x.
//!
//!   benchmark                  kind     spec/tw   spec/gen
//!   fib(30) recursion          compute    4.97x     1.02x
//!   sum recursion (500 x2000)  compute    5.79x     1.10x
//!   numeric loop (1e6)         compute    2.87x     1.23x
//!   while loop (1e6)           compute    3.32x     1.42x
//!   property r/w (1e6)         compute    3.00x     1.57x
//!   method dispatch (1e6)      compute    2.90x     1.87x
//!   string concat (50000)      alloc      1.25x     1.05x   (EXEMPT from >= 2x)
//!   template build (50000)     alloc      1.09x     0.99x   (EXEMPT)
//!   closure capture (1e6)      compute    4.26x     1.13x   (NEW — SP8 #136)
//!   geomean spec/tw = 2.88x   (A0 baseline 2.73x → 2.88x; V11-T6 was 2.92x)
//!
//! ───────────────────────────────────────────────────────────────────────────────
//! DBG TASK 9 — ZERO-COST GATE (the spec §3.4 PRIMARY ACCEPTANCE artifact). The post-DBG
//! VM with NO debugger/profiler attached (`instrument == None`, the production path) must
//! be a STATISTICAL NO-OP vs the same VM with an EMPTY instrumentation armed
//! (`instrument == Some`, every sub-feature `None` — the "attached but idle" config). The
//! `dbg_zero_cost_gate` section times both over the whole corpus and gates the geomean.
//!
//! GATE RESULT — PASS (recorded DBG Task 9, 2026-06-10, release):
//!   - spec/tw geomean = **2.95x** (the standing >= 2x gate; ABOVE the pre-DBG SP8 2.88x
//!     baseline → the new `publish_profile_frames` None-check at frame push/pop added NO
//!     measurable cost to the `instrument == None` production path — config (1)≈(2)).
//!   - armed/none geomean = **0.998x** (every bench within ±1.4% noise → arming an empty
//!     instrumentation is free; the `Op::Break` trap arm is unreachable when no byte is
//!     patched, and the push/pop publish is a single `Option` None-check — config (2)≈(3)).
//!
//! A non-noise gap in armed/none would mean a stray instrumentation check leaked into a
//! hot path: a BUG to fix, never an accepted tradeoff. The `assert!` in
//! `dbg_zero_cost_gate` enforces it (bound 1.08x — generous for machine noise).
//!
//!   (a) COMPUTE-BOUND >= 2x spec/tw: PASS (all 7 compute benches, min 2.87x).
//!   (b) NO spec-vs-generic regression: PASS (every bench; alloc benches at ~1.0x
//!       spec/gen with allocator jitter).
//!   (c) The new `closure capture` bench (a closure capturing a never-reassigned `k`
//!       each iteration — by-value eligible) lands at 4.26x spec/tw, 1.13x spec/gen.
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
    /// DBG Task 9: the SPECIALIZED VM with an EMPTY `Instrumentation` armed
    /// (`instrument == Some`, all sub-features `None`; no byte patched, profiler off).
    /// The "attached debugger, idle" config — its time must be within noise of
    /// `SpecializedVm` (`instrument == None`), the zero-cost-when-off acceptance gate.
    ArmedIdleVm,
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
        Engine::ArmedIdleVm => {
            ascript::vm_run_source_armed_idle(src)
                .await
                .unwrap_or_else(|e| panic!("armed-idle VM failed on `{name}`: {e}"));
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
        // ── closure capture (SP8 #136 capture-by-value) ──────────────────────
        // Each iteration builds a closure capturing a NEVER-reassigned local `k`.
        // Under SP8 #136 `k` is captured BY VALUE: no cell allocation in `make`'s
        // declaring frame and a plain GET_LOCAL (no RefCell borrow), vs the old
        // per-iteration cell allocation + borrow.
        Bench {
            name: "closure capture (1e6)",
            compute_bound: true,
            src: r#"
fn make(base) {
  let k = base + 1
  return () => k
}
let total = 0
for (i in 0..1000000) {
  let f = make(i)
  total = total + f()
}
print(total)
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

    dbg_zero_cost_gate(&benches).await;
}

/// DBG Task 9 — the PRIMARY ACCEPTANCE GATE (spec §3.4): the post-DBG VM with NO
/// debugger/profiler attached (`instrument == None`, the production path) must be a
/// STATISTICAL NO-OP versus the same VM with an EMPTY instrumentation armed
/// (`instrument == Some`, all sub-features `None` — the "attached but idle" config).
///
/// The two configs differ ONLY in:
///   - the per-CALL `publish_profile_frames` None-check at frame push/pop sees `Some`
///     (and `profiler == None`) instead of `None` — a single extra `Option` deref;
///   - the `Op::Break` trap arm is present in both but UNREACHABLE in both (no byte is
///     patched), so the per-INSTRUCTION dispatch loop is byte-identical.
///
/// So a non-noise gap here would mean a stray instrumentation check leaked into a hot
/// path — a BUG to fix, never an accepted tradeoff. The compute corpus (esp. the
/// recursion benches, which push/pop frames hardest) is the stress.
async fn dbg_zero_cost_gate(benches: &[Bench]) {
    println!("DBG ZERO-COST GATE (Task 9 §3.4): instrument==None vs armed-idle (Some, all None)");
    println!(
        "{:<28} {:>11} {:>11} {:>12}",
        "benchmark", "none (ms)", "armed (ms)", "armed/none",
    );
    println!("{}", "-".repeat(28 + 11 * 2 + 12 + 3));

    let mut ratios: Vec<f64> = Vec::new();
    for b in benches {
        // `SpecializedVm` == production (`instrument == None`); `ArmedIdleVm` == Some(empty).
        let (none_med, _) = measure(Engine::SpecializedVm, b.src, b.name).await;
        let (armed_med, _) = measure(Engine::ArmedIdleVm, b.src, b.name).await;
        let ratio = armed_med.as_secs_f64() / none_med.as_secs_f64();
        ratios.push(ratio);
        println!(
            "{:<28} {} {} {:>11.3}x",
            b.name,
            ms(none_med),
            ms(armed_med),
            ratio,
        );
    }

    let geomean = (ratios.iter().map(|r| r.ln()).sum::<f64>() / ratios.len() as f64).exp();
    println!();
    println!("geomean armed/none = {geomean:.3}x  (1.000x = perfect zero-cost; >1 = idle overhead)");

    // PASS condition: the armed-idle geomean is within timing noise of not-attached.
    // The seams are `None`-gated (a single Option check at push/pop), so the only
    // expected gap is sub-percent (measured geomean ~1.000x, worst single bench ~1.017x).
    // A 5% bound passes ordinary machine noise but trips a GENUINE leaked check sooner
    // (a per-instruction check would be 1.3x+) — tightened from the initial 1.08x per the
    // holistic review (the looser bound could mask a 2-7% armed-idle regression).
    const ZERO_COST_BOUND: f64 = 1.05;
    if geomean <= ZERO_COST_BOUND {
        println!("  [PASS] armed-idle is within noise of not-attached → zero-cost-when-off holds");
    } else {
        println!("  [FAIL] armed-idle geomean {geomean:.3}x exceeds {ZERO_COST_BOUND:.2}x — a");
        println!("         stray instrumentation check leaked into a hot path; fix it (do NOT relax).");
    }
    assert!(
        geomean <= ZERO_COST_BOUND,
        "DBG zero-cost gate FAILED: armed-idle geomean {geomean:.3}x > {ZERO_COST_BOUND:.2}x \
         (instrumentation overhead leaked into a hot path)"
    );
    println!();
}
