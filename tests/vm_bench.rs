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

use std::time::{Duration, Instant};

/// Number of timed runs per (program, engine). The median and min are reported.
const RUNS: usize = 7;

/// Which engine to time.
#[derive(Clone, Copy)]
enum Engine {
    TreeWalker,
    Vm,
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
        Engine::Vm => {
            ascript::vm_run_source(src)
                .await
                .unwrap_or_else(|e| panic!("VM failed on `{name}`: {e}"));
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

/// A single compute-bound benchmark program.
struct Bench {
    name: &'static str,
    src: &'static str,
}

/// The compute-bound, import-free benchmark corpus.
fn benches() -> Vec<Bench> {
    vec![
        // ── deep / heavy recursion ──────────────────────────────────────────
        Bench {
            name: "fib(30) recursion",
            src: r#"
fn fib(n) {
  if (n < 2) { return n }
  return fib(n - 1) + fib(n - 2)
}
print(fib(30))
"#,
        },
        Bench {
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
            src: r#"
let sum = 0
for (i in 0..1000000) { sum = sum + i }
print(sum)
"#,
        },
        Bench {
            name: "while loop (1e6)",
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
            src: r#"
let s = ""
for (i in 0..50000) { s = s + "x" }
print(len(s))
"#,
        },
        Bench {
            name: "template build (50000)",
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
    println!("VM-vs-tree-walker baseline (generic/un-specialized VM)");
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
        "{:<28} {:>12} {:>12} {:>12} {:>12} {:>9}",
        "benchmark", "tw med (ms)", "vm med (ms)", "tw min (ms)", "vm min (ms)", "speedup"
    );
    println!("{}", "-".repeat(28 + 12 * 4 + 9 + 5));

    let mut ratios: Vec<f64> = Vec::new();
    for b in &benches {
        let (tw_med, tw_min) = measure(Engine::TreeWalker, b.src, b.name).await;
        let (vm_med, vm_min) = measure(Engine::Vm, b.src, b.name).await;

        let speedup = tw_med.as_secs_f64() / vm_med.as_secs_f64();
        ratios.push(speedup);

        println!(
            "{:<28} {} {} {} {} {:>8.2}x",
            b.name,
            ms(tw_med),
            ms(vm_med),
            ms(tw_min),
            ms(vm_min),
            speedup
        );
    }

    println!();
    let geomean = ratios
        .iter()
        .map(|r| r.ln())
        .sum::<f64>()
        / ratios.len() as f64;
    let geomean = geomean.exp();
    let n_at_2x = ratios.iter().filter(|&&r| r >= 2.0).count();
    println!(
        "geomean speedup = {geomean:.2}x   ({n_at_2x}/{} benches at >= 2.0x)",
        ratios.len()
    );
    println!();
}
