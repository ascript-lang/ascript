//! End-to-end tests for Workers Spec B stateful workers — Task 5 (actors).
//!
//! Each test writes a `.as` program to a temp file and runs it through the built
//! binary on BOTH engines (the default bytecode VM AND `--tree-walker`), asserting
//! byte-identical stdout — the cross-engine parity invariant for the actor runtime.

use std::process::Command;

/// Run `src` as a `.as` program on a given engine. `tree_walker` selects the legacy
/// oracle engine (`--tree-walker` flag precedes the file). Returns (success, stdout,
/// stderr).
fn run_engine(name: &str, src: &str, tree_walker: bool) -> (bool, String, String) {
    let suffix = if tree_walker { "tw" } else { "vm" };
    let file = std::env::temp_dir().join(format!("ascript_workers_stateful_{name}_{suffix}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    cmd.arg(&file);
    let output = cmd.output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Assert a program prints exactly `expected` on BOTH engines.
fn assert_both_engines(name: &str, src: &str, expected: &str) {
    for tw in [false, true] {
        let engine = if tw { "tree-walker" } else { "vm" };
        let (ok, out, err) = run_engine(name, src, tw);
        assert!(ok, "[{engine}] process failed: stdout={out:?} stderr={err:?}");
        assert_eq!(out, expected, "[{engine}] stdout mismatch (stderr={err:?})");
    }
}

#[test]
fn actor_counter_state_persists() {
    // A `worker class` actor: spawned into its own isolate, state persists across
    // method calls, talked to via an async proxy handle.
    let src = r#"
worker class Counter {
  n: number = 0
  fn inc(): number { self.n = self.n + 1; return self.n }
  fn get(): number { return self.n }
}
async fn main() {
  let c = await Counter.spawn()
  print(await c.inc())   // 1
  print(await c.inc())   // 2
  print(await c.get())   // 2
  c.close()
}
await main()
"#;
    assert_both_engines("counter", src, "1\n2\n2\n");
}

#[test]
fn actor_spawn_passes_init_args() {
    // `spawn(args)` ships the args to the isolate, which runs `init` IN the isolate.
    let src = r#"
worker class Greeter {
  prefix: string = ""
  fn init(p) { self.prefix = p }
  fn hello(name): string { return self.prefix + name }
}
async fn main() {
  let g = await Greeter.spawn("Hi, ")
  print(await g.hello("Ada"))
  print(await g.hello("Bob"))
  g.close()
}
await main()
"#;
    assert_both_engines("greeter", src, "Hi, Ada\nHi, Bob\n");
}

#[test]
fn actor_local_construction_still_works() {
    // A bare `ClassName(args)` on a `worker class` still builds a LOCAL instance
    // (construction is NOT overloaded by `spawn`).
    let src = r#"
worker class Box {
  v: number = 0
  fn set(x): number { self.v = x; return self.v }
}
let b = Box()
print(b.set(7))
print(b.v)
"#;
    assert_both_engines("local", src, "7\n7\n");
}

#[test]
fn actor_closed_call_is_recoverable() {
    // A call on a closed actor resolves to a RECOVERABLE panic ("actor is closed"),
    // catchable by `recover`.
    let src = r#"
worker class Counter {
  n: number = 0
  fn inc(): number { self.n = self.n + 1; return self.n }
}
async fn main() {
  let c = await Counter.spawn()
  print(await c.inc())   // 1
  c.close()
  let r = recover(() => await c.inc())
  print(r[1] != nil)     // true: an error was caught
}
await main()
"#;
    assert_both_engines("closed", src, "1\ntrue\n");
}

#[test]
fn actor_method_panic_is_recoverable() {
    // An uncaught Tier-2 panic inside an actor method re-raises as a recoverable
    // panic on the caller.
    let src = r#"
worker class Risky {
  fn boom(): number { panic("kaboom") }
  fn ok(): number { return 42 }
}
async fn main() {
  let r = await Risky.spawn()
  let caught = recover(() => await r.boom())
  print(caught[1] != nil)   // true
  print(await r.ok())       // 42: actor still alive after a recovered method panic
  r.close()
}
await main()
"#;
    assert_both_engines("panic", src, "true\n42\n");
}

#[test]
fn actor_non_sendable_arg_panics() {
    // Passing a non-sendable arg (a function) to an actor method is a recoverable
    // sendability panic.
    let src = r#"
worker class Counter {
  n: number = 0
  fn take(f): number { return self.n }
}
async fn main() {
  let c = await Counter.spawn()
  let r = recover(() => await c.take(() => 1))
  print(r[1] != nil)   // true: sendability error caught
  c.close()
}
await main()
"#;
    assert_both_engines("nonsendable", src, "true\n");
}

#[cfg(feature = "sql")]
#[test]
fn actor_owns_native_resource() {
    // The "resource lives in the actor" pattern: `init` opens an in-memory sqlite
    // connection INSIDE the isolate; a method queries it. The resource never crosses
    // the boundary — only data does.
    let src = r#"
import { open } from "std/sqlite"
worker class Store {
  db: any?
  fn init() {
    self.db = open(":memory:")[0]
    self.db.exec("CREATE TABLE kv (k TEXT, v INTEGER)")
  }
  fn put(k, v): number { self.db.exec("INSERT INTO kv VALUES (?, ?)", [k, v]); return v }
  fn total(): number {
    let rows = self.db.query("SELECT v FROM kv")[0]
    let sum = 0
    for (row in rows) { sum = sum + row.v }
    return sum
  }
}
async fn main() {
  let s = await Store.spawn()
  print(await s.put("a", 10))
  print(await s.put("b", 32))
  print(await s.total())   // 42
  s.close()
}
await main()
"#;
    assert_both_engines("resource", src, "10\n32\n42\n");
}

#[cfg(feature = "sql")]
#[test]
fn actor_returning_raw_resource_is_sendability_panic() {
    // Returning the raw native resource handle from a method is a sendability panic
    // (it can't cross — methods must return data, not the isolate-local resource).
    let src = r#"
import { open } from "std/sqlite"
worker class Store {
  db: any?
  fn init() { self.db = open(":memory:")[0] }
  fn leak() { return self.db }
}
async fn main() {
  let s = await Store.spawn()
  let r = recover(() => await s.leak())
  print(r[1] != nil)   // true: the native handle cannot be returned across
  s.close()
}
await main()
"#;
    assert_both_engines("leak", src, "true\n");
}

// ---------------------------------------------------------------------------
// Task 6 — `worker fn*` STREAMING generators (dedicated isolate, demand-driven
// pull, bounded buffer, bidirectional next(v), close/drop teardown).
// ---------------------------------------------------------------------------

#[test]
fn stream_records_yields_ordered_sequence() {
    // A `worker fn*` runs its producer body in a dedicated isolate and streams its
    // ordered sequence back, consumed transparently via `for await`. Each yielded
    // value crosses the boundary via structured-clone encode/decode.
    let src = r#"
worker fn* records(n) { for (i in 1..=n) { yield i * 10 } }
async fn main() {
  for await (r in records(3)) { print(r) }
}
await main()
"#;
    assert_both_engines("stream_records", src, "10\n20\n30\n");
}

#[test]
fn stream_yields_structured_values() {
    // Yielded OBJECTS cross the boundary intact (structured clone), proving the
    // serializer round-trips non-scalar yields.
    let src = r#"
worker fn* recs(n) { for (i in 1..=n) { yield { id: i, label: `rec-${i}` } } }
async fn main() {
  for await (r in recs(2)) { print(`${r.id}:${r.label}`) }
}
await main()
"#;
    assert_both_engines("stream_structured", src, "1:rec-1\n2:rec-2\n");
}

#[test]
fn stream_backpressure_strict_pull() {
    // STRICT PULL (prefetch=1): the producer advances at most one step per demand
    // credit, so an INFINITE producer consumed only a few times terminates cleanly.
    // If the producer ran ahead of demand unbounded, this would hang forever — so a
    // clean, bounded result IS the backpressure assertion.
    let src = r#"
worker fn* naturals() { let i = 0; while (true) { i = i + 1; yield i } }
async fn main() {
  let g = naturals()
  print(await g.next())   // 1
  print(await g.next())   // 2
  print(await g.next())   // 3
  g.close()
}
await main()
print("done")
"#;
    assert_both_engines("stream_backpressure", src, "1\n2\n3\ndone\n");
}

#[test]
fn stream_bidirectional_next_value() {
    // A value passed to `.next(v)` is injected back across the boundary as the result
    // of the producer's suspended `yield` expression — bidirectional round-trip.
    let src = r#"
worker fn* echo() { let a = yield 1; let b = yield a + 100; yield b + 1000 }
async fn main() {
  let g = echo()
  print(await g.next())    // 1   (first next ignores its input)
  print(await g.next(5))   // 105 (a = 5)
  print(await g.next(7))   // 1007 (b = 7)
}
await main()
"#;
    assert_both_engines("stream_bidi", src, "1\n105\n1007\n");
}

#[test]
fn stream_close_then_next_is_done() {
    // `close()` tears the isolate down; a subsequent `.next()` is the done sentinel
    // (nil). The process exits cleanly afterward (no zombie producer thread).
    let src = r#"
worker fn* records(n) { for (i in 1..=n) { yield i } }
async fn main() {
  let g = records(5)
  print(await g.next())   // 1
  print(await g.next())   // 2
  g.close()
  print(await g.next())   // nil (done after close)
}
await main()
print("done")
"#;
    assert_both_engines("stream_close", src, "1\n2\nnil\ndone\n");
}

#[test]
fn stream_drop_without_close_tears_down() {
    // Dropping the generator (going out of scope, never `close`d, partially consumed)
    // reclaims the producer isolate via last-drop teardown — the process still exits
    // cleanly (the trailing "done" prints, proving no exit-drain hang on a zombie).
    let src = r#"
worker fn* naturals() { let i = 0; while (true) { i = i + 1; yield i } }
async fn main() {
  let g = naturals()
  print(await g.next())   // 1
  // g goes out of scope here without close() — last-drop tears the isolate down.
}
await main()
print("done")
"#;
    assert_both_engines("stream_drop", src, "1\ndone\n");
}

#[test]
fn stream_producer_panic_is_recoverable() {
    // An uncaught Tier-2 panic inside the producer body surfaces as a RECOVERABLE
    // panic on the consumer (catchable by `recover`), after earlier yields succeed.
    let src = r#"
worker fn* flaky() { yield 1; panic("boom"); yield 2 }
async fn main() {
  let g = flaky()
  print(await g.next())                         // 1
  let r = recover(() => await g.next())
  print(r[1] != nil)                            // true: producer panic caught
}
await main()
"#;
    assert_both_engines("stream_panic", src, "1\ntrue\n");
}

#[test]
fn stream_non_sendable_yield_panics() {
    // A non-sendable yielded value (a function) cannot cross the boundary — it is a
    // recoverable sendability panic with a field path on the consumer.
    let src = r#"
worker fn* bad() { yield (() => 1) }
async fn main() {
  let g = bad()
  let r = recover(() => await g.next())
  print(r[1] != nil)   // true: sendability error caught
}
await main()
"#;
    assert_both_engines("stream_nonsendable_yield", src, "true\n");
}

#[test]
fn stream_non_sendable_arg_panics() {
    // A non-sendable CALL arg to a `worker fn*` is a recoverable sendability panic at
    // the call site (before any value streams).
    let src = r#"
worker fn* take(f) { yield 1 }
async fn main() {
  let r = recover(() => take(() => 1))
  print(r[1] != nil)   // true: sendability error caught
}
await main()
"#;
    assert_both_engines("stream_nonsendable_arg", src, "true\n");
}

// ---------------------------------------------------------------------------
// Task 7 — `task.pipe(gen, bus)` bridge helper
// ---------------------------------------------------------------------------

#[test]
fn pipe_bridge_fans_out_in_order() {
    // Basic bridge: pipe a worker generator's items onto a local event bus.
    // Items arrive in order; the bus listener receives them with correct field values.
    let src = r#"
import { pipe } from "std/task"
import * as events from "std/events"
worker fn* source() { yield {kind:"a", n:1}; yield {kind:"a", n:2} }
async fn main() {
  let bus = events.new()
  let seen = []
  bus.on("a", (e) => { seen = [...seen, e.n] })
  await pipe(source(), bus)
  print(seen)
}
await main()
"#;
    assert_both_engines("bridge_basic", src, "[1, 2]\n");
}

#[test]
fn pipe_bridge_multi_listener_both_receive_in_order() {
    // Multi-listener: two `on("a", ...)` listeners both observe both events in order.
    let src = r#"
import { pipe } from "std/task"
import * as events from "std/events"
worker fn* source() { yield {kind:"a", n:10}; yield {kind:"a", n:20} }
async fn main() {
  let bus = events.new()
  let seenA = []
  let seenB = []
  bus.on("a", (e) => { seenA = [...seenA, e.n] })
  bus.on("a", (e) => { seenB = [...seenB, e.n] })
  await pipe(source(), bus)
  print(seenA)
  print(seenB)
}
await main()
"#;
    assert_both_engines("bridge_multi", src, "[10, 20]\n[10, 20]\n");
}

#[test]
fn pipe_bridge_slow_listener_still_receives_all() {
    // Backpressure: a slow listener (awaits a short sleep) still receives all events
    // and the final result is complete and ordered. Slowness threads back through
    // emit → consume loop → resume → producer (demand-driven pull).
    let src = r#"
import { pipe } from "std/task"
import * as events from "std/events"
import { sleep } from "std/time"
worker fn* source() { yield {kind:"a", n:1}; yield {kind:"a", n:2}; yield {kind:"a", n:3} }
async fn main() {
  let bus = events.new()
  let seen = []
  bus.on("a", async (e) => {
    await sleep(1)
    seen = [...seen, e.n]
  })
  await pipe(source(), bus)
  print(seen)
}
await main()
"#;
    assert_both_engines("bridge_slow", src, "[1, 2, 3]\n");
}

// ---------------------------------------------------------------------------
// Task 11 — the 4th execution mode: a `worker class` actor AND a `worker fn*`
// streaming generator must work when run from a COMPILED `.aso` file (no source
// available). The slice is rebuilt from the stored `.aso` bytes
// (`Interp::worker_aso_bytes`) — Plan A Task 15's mechanism extended to actor
// spawn and the worker-generator stream path.
// ---------------------------------------------------------------------------

/// Build `src` to a `.aso` with `ascript build`, then run the `.aso` with
/// `ascript run`. Returns (success, stdout, stderr) of the run step.
fn build_then_run_aso(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir();
    let as_file = dir.join(format!("ascript_workers_aso_{name}.as"));
    let aso_file = dir.join(format!("ascript_workers_aso_{name}.aso"));
    std::fs::write(&as_file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");

    let build = Command::new(bin)
        .arg("build")
        .arg(&as_file)
        .arg("-o")
        .arg(&aso_file)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "[build] failed: stderr={}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(bin).arg("run").arg(&aso_file).output().unwrap();
    (
        run.status.success(),
        String::from_utf8_lossy(&run.stdout).to_string(),
        String::from_utf8_lossy(&run.stderr).to_string(),
    )
}

#[test]
fn aso_mode_worker_class_actor_spawn() {
    // A `worker class` spawned + driven from a compiled `.aso` (no source on the
    // Interp): the class slice is rebuilt from the stored `.aso` bytes.
    let src = r#"
worker class Counter {
  n: number = 0
  fn inc(): number { self.n = self.n + 1; return self.n }
  fn get(): number { return self.n }
}
async fn main() {
  let c = await Counter.spawn()
  print(await c.inc())
  print(await c.inc())
  print(await c.get())
  c.close()
}
await main()
"#;
    let (ok, out, err) = build_then_run_aso("counter", src);
    assert!(ok, "[.aso run] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "1\n2\n2\n", "[.aso run] stdout mismatch (stderr={err:?})");
}

#[test]
fn aso_mode_worker_class_actor_with_init_args() {
    // Init args ship to the isolate and `init` runs there — from a `.aso` too.
    let src = r#"
worker class Greeter {
  prefix: string = ""
  fn init(p) { self.prefix = p }
  fn hello(name): string { return self.prefix + name }
}
async fn main() {
  let g = await Greeter.spawn("Hi, ")
  print(await g.hello("Ada"))
  print(await g.hello("Bob"))
  g.close()
}
await main()
"#;
    let (ok, out, err) = build_then_run_aso("greeter", src);
    assert!(ok, "[.aso run] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "Hi, Ada\nHi, Bob\n", "[.aso run] stdout mismatch (stderr={err:?})");
}

#[test]
fn aso_mode_worker_generator_stream() {
    // A `worker fn*` streamed from a compiled `.aso` (no source): the producer
    // slice is rebuilt from the stored `.aso` bytes.
    let src = r#"
worker fn* records(n) { for (i in 1..=n) { yield i * 10 } }
async fn main() {
  for await (r in records(3)) { print(r) }
}
await main()
"#;
    let (ok, out, err) = build_then_run_aso("records", src);
    assert!(ok, "[.aso run] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "10\n20\n30\n", "[.aso run] stdout mismatch (stderr={err:?})");
}

#[test]
fn aso_mode_worker_generator_bidirectional() {
    // Bidirectional `next(v)` resume across the isolate boundary, from a `.aso`.
    let src = r#"
worker fn* echo() { let a = yield 1; let b = yield a + 100; yield b + 1000 }
async fn main() {
  let g = echo()
  print(await g.next())
  print(await g.next(5))
  print(await g.next(7))
}
await main()
"#;
    let (ok, out, err) = build_then_run_aso("echo", src);
    assert!(ok, "[.aso run] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "1\n105\n1007\n", "[.aso run] stdout mismatch (stderr={err:?})");
}

// ---------------------------------------------------------------------------
// Task 12: SP9 determinism — cross-isolate boundary record/replay.
//
// A `worker fn*` consumed INSIDE a `workflow.run` body is event-sourced (each
// yielded value crossing the isolate boundary becomes a `GeneratorYield` event in
// the workflow log). On `workflow.resume` after a crash (the completion line is
// missing), the workflow body re-runs but the generator's yields are REPLAYED from
// the log WITHOUT re-driving the producer isolate.
//
// DECISIVE PROOF: between record and resume we CHANGE the producer body (so a real
// re-drive would yield different values). Because replay returns the RECORDED yields
// from the log, the resumed run produces the ORIGINAL sum — proving the producer
// isolate was not re-driven for the recorded prefix.
//
// (Actors are exercised by the in-crate det.rs unit tests + the same hook; the
// generator path is the harder one and is what this end-to-end test pins.)
// ---------------------------------------------------------------------------

/// Run a `.as` `src` through the binary on the default VM engine, with a stable file
/// name so an associated workflow log path is predictable. Returns (ok, stdout, stderr).
#[cfg(feature = "workflow")]
fn run_vm_named(file: &std::path::Path, src: &str) -> (bool, String, String) {
    std::fs::write(file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(file).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[cfg(feature = "workflow")]
#[test]
fn determinism_replays_worker_stream_yields_in_workflow() {
    let dir = std::env::temp_dir();
    let as_file = dir.join("ascript_workers_det_stream.as");
    let log_file = dir.join("ascript_workers_det_stream.log");
    let _ = std::fs::remove_file(&log_file);

    // PHASE 1 — RECORD: a workflow body consumes a `worker fn*`. Each yield crosses
    // the isolate boundary and is event-sourced into the log.
    let log_path = log_file.to_string_lossy().replace('\\', "/");
    let record_src = format!(
        r#"
import {{ run }} from "std/workflow"
worker fn* records(n) {{ for (i in 1..=n) {{ yield i * 10 }} }}
async fn body(ctx, input) {{
  let total = 0
  for await (r in records(3)) {{ total = total + r }}
  return total
}}
async fn main() {{
  let result = await run(body, 0, {{ log: "{log_path}" }})
  print(result)
}}
await main()
"#
    );
    let (ok, out, err) = run_vm_named(&as_file, &record_src);
    assert!(ok, "[record] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "60\n", "[record] real producer sums 10+20+30 (stderr={err:?})");

    // The log must contain the event-sourced generator yields.
    let log = std::fs::read_to_string(&log_file).expect("workflow log written");
    let yield_lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("\"GeneratorYield\""))
        .collect();
    assert!(
        yield_lines.len() >= 3,
        "expected >=3 GeneratorYield events in the log, got {}:\n{log}",
        yield_lines.len()
    );

    // PHASE 2 — DOCTOR + RESUME: strip the completion line so `resume` RE-RUNS the body.
    let doctored: String = log
        .lines()
        .filter(|l| !l.contains("\"WorkflowCompleted\""))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&log_file, &doctored).unwrap();

    // Resume with a CHANGED producer (`i * 1000` instead of `i * 10`). If resume
    // re-drove the producer isolate it would sum 1000+2000+3000 = 6000; because the
    // yields are REPLAYED from the recorded log, the sum is the ORIGINAL 60 — decisive
    // proof the boundary is not re-crossed for the recorded prefix.
    let resume_src = format!(
        r#"
import {{ resume }} from "std/workflow"
worker fn* records(n) {{ for (i in 1..=n) {{ yield i * 1000 }} }}
async fn body(ctx, input) {{
  let total = 0
  for await (r in records(3)) {{ total = total + r }}
  return total
}}
async fn main() {{
  let result = await resume(body, 0, {{ log: "{log_path}" }})
  print(result)
}}
await main()
"#
    );
    let (ok, out, err) = run_vm_named(&as_file, &resume_src);
    assert!(ok, "[resume] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(
        out, "60\n",
        "[resume] replays the recorded yields (NOT the changed producer's 6000) (stderr={err:?})"
    );

    let _ = std::fs::remove_file(&log_file);
}

#[cfg(feature = "workflow")]
#[test]
fn determinism_replays_actor_messages_in_workflow() {
    let dir = std::env::temp_dir();
    let as_file = dir.join("ascript_workers_det_actor.as");
    let log_file = dir.join("ascript_workers_det_actor.log");
    let _ = std::fs::remove_file(&log_file);
    let log_path = log_file.to_string_lossy().replace('\\', "/");

    // PHASE 1 — RECORD: a workflow body spawns an actor and calls a method; the reply
    // crosses the isolate boundary and is event-sourced into the log as `ActorCall`.
    let record_src = format!(
        r#"
import {{ run }} from "std/workflow"
worker class Adder {{
  base: number = 0
  fn init(b) {{ self.base = b }}
  fn add(x): number {{ return self.base + x }}
}}
async fn body(ctx, input) {{
  let a = await Adder.spawn(100)
  let r = await a.add(5)
  a.close()
  return r
}}
async fn main() {{
  print(await run(body, 0, {{ log: "{log_path}" }}))
}}
await main()
"#
    );
    let (ok, out, err) = run_vm_named(&as_file, &record_src);
    assert!(ok, "[record] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "105\n", "[record] real actor returns 100+5 (stderr={err:?})");

    let log = std::fs::read_to_string(&log_file).expect("workflow log written");
    assert!(
        log.lines().any(|l| l.contains("\"ActorCall\"")),
        "expected an ActorCall event in the log:\n{log}"
    );

    // PHASE 2 — DOCTOR + RESUME: strip the completion line so `resume` re-runs the body.
    let doctored: String = log
        .lines()
        .filter(|l| !l.contains("\"WorkflowCompleted\""))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&log_file, &doctored).unwrap();

    // Resume with a CHANGED actor method (`base * x` instead of `base + x`). A real
    // re-cross would return 100*5 = 500; replay-from-log returns the recorded 105.
    let resume_src = format!(
        r#"
import {{ resume }} from "std/workflow"
worker class Adder {{
  base: number = 0
  fn init(b) {{ self.base = b }}
  fn add(x): number {{ return self.base * x }}
}}
async fn body(ctx, input) {{
  let a = await Adder.spawn(100)
  let r = await a.add(5)
  a.close()
  return r
}}
async fn main() {{
  print(await resume(body, 0, {{ log: "{log_path}" }}))
}}
await main()
"#
    );
    let (ok, out, err) = run_vm_named(&as_file, &resume_src);
    assert!(ok, "[resume] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(
        out, "105\n",
        "[resume] replays the recorded ActorCall (NOT the changed method's 500) (stderr={err:?})"
    );

    let _ = std::fs::remove_file(&log_file);
}

// ---------------------------------------------------------------------------
// Worker-FUNCTION code-slice follow-ups (fix/workers-followups): a `worker fn`
// body may now use (1) top-level stdlib imports, (2) top-level classes/enums it
// constructs/returns, and (3) computed-initializer top-level consts. All must run
// byte-identically on BOTH engines and survive `.aso` build→run.
// ---------------------------------------------------------------------------

#[test]
fn worker_fn_uses_stdlib_import() {
    // A `worker fn` body that calls an imported stdlib module fn (`math.max`).
    let src = r#"
import * as math from "std/math"
worker fn mx(a: number, b: number, c: number): number {
  return math.max(a, b, c)
}
async fn main() {
  print(await mx(3, 9, 5))
}
await main()
"#;
    assert_both_engines("import_math", src, "9\n");
}

#[test]
fn worker_fn_uses_transitive_import_via_helper() {
    // The import is used by a top-level helper fn the worker calls transitively.
    let src = r#"
import * as array from "std/array"
fn firstSorted(xs: array<number>): number {
  let s = array.sort(xs)
  return s[0]
}
worker fn minOf(xs: array<number>): number {
  return firstSorted(xs)
}
async fn main() {
  print(await minOf([5, 2, 9, 1, 7]))
}
await main()
"#;
    assert_both_engines("import_transitive", src, "1\n");
}

#[test]
fn worker_fn_constructs_and_returns_class() {
    // A `worker fn` constructs a top-level class instance and returns it; the
    // instance round-trips back via structured-clone (field access on the caller).
    let src = r#"
class Point {
  x: number
  y: number
  fn init(x, y) { self.x = x; self.y = y }
}
worker fn mk(a: number, b: number): Point {
  return Point(a, b)
}
async fn main() {
  let p = await mk(3, 4)
  print(p.x + p.y)
}
await main()
"#;
    assert_both_engines("class_return", src, "7\n");
}

#[test]
fn worker_fn_class_with_superclass() {
    // A worker fn constructs a subclass; the superclass chain must ship too.
    let src = r#"
class Shape {
  kind: string
  fn init(k) { self.kind = k }
}
class Circle extends Shape {
  r: number
  fn init(r) { super.init("circle"); self.r = r }
}
worker fn mkCircle(r: number): Circle {
  return Circle(r)
}
async fn main() {
  let c = await mkCircle(5)
  print(c.kind)
  print(c.r)
}
await main()
"#;
    assert_both_engines("class_super", src, "circle\n5\n");
}

#[test]
fn worker_fn_uses_enum() {
    // A worker fn reads a top-level enum variant (enums already ship as const values,
    // but assert it stays working alongside the new import/class shipping).
    let src = r#"
enum Color { Red, Green, Blue }
worker fn pick(): Color { return Color.Green }
async fn main() {
  print(await pick())
}
await main()
"#;
    assert_both_engines("enum_use", src, "Color.Green\n");
}

#[test]
fn worker_fn_uses_computed_const() {
    // A worker fn references a top-level `const` whose initializer is a computed
    // expression (a function call), not a literal. The initializer code ships into
    // the slice and is recomputed on the isolate.
    let src = r#"
fn expensive(): number { return 21 * 2 }
const K = expensive()
worker fn rd(n: number): number { return K + n }
async fn main() {
  print(await rd(8))
}
await main()
"#;
    assert_both_engines("computed_const", src, "50\n");
}

#[test]
fn worker_fn_computed_const_uses_import() {
    // A computed const whose initializer itself uses an imported stdlib module.
    let src = r#"
import * as math from "std/math"
const M = math.max(10, 20, 15)
worker fn rd(n: number): number { return M + n }
async fn main() {
  print(await rd(5))
}
await main()
"#;
    assert_both_engines("computed_const_import", src, "25\n");
}

#[test]
fn aso_mode_worker_fn_import_class_const() {
    // The combined import + class + computed-const slice must survive build→run .aso
    // (the slice is rebuilt from the stored `.aso` bytes, no source on the Interp).
    let src = r#"
import * as math from "std/math"
class Box {
  v: number
  fn init(v) { self.v = v }
}
fn base(): number { return math.max(1, 7, 3) }
const K = base()
worker fn make(n: number): Box {
  return Box(K + n)
}
async fn main() {
  let b = await make(10)
  print(b.v)
}
await main()
"#;
    let (ok, out, err) = build_then_run_aso("fn_import_class_const", src);
    assert!(ok, "[.aso run] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "17\n", "[.aso run] stdout mismatch (stderr={err:?})");
}

#[test]
fn worker_fn_computed_const_with_internal_jumps() {
    // A computed-const initializer containing ternary expressions (internal relative
    // jumps) — proves jump displacements survive the contiguous range copy into the
    // worker code slice.
    let src = r#"
fn pick(b: bool): number { return b ? 100 : 200 }
const K = pick(true) + (5 > 3 ? 1 : 2)
worker fn rd(n: number): number { return K + n }
async fn main() {
  print(await rd(0))
}
await main()
"#;
    assert_both_engines("computed_const_jumps", src, "101\n");
}

// ---------------------------------------------------------------------------
// Regression: a computed-const slice range must be EXACTLY the const's own
// initializer — never absorb a preceding non-defining statement (a `for`/`while`
// loop, a bare expression statement, or an `if`). Absorbing a loop shipped its
// backward `Loop` + `GET_LOCAL` into the fragment whose top-level slot_count was too
// small → a hard `set_local slot out of bounds` isolate panic; absorbing a stack-
// neutral statement over-shipped + re-ran its side effects on the isolate.
// ---------------------------------------------------------------------------

#[test]
fn worker_fn_computed_const_after_for_loop() {
    let src = r#"
fn expensive(): number { return 42 }
for (i in 0..3) { i + 1 }
const K = expensive()
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    assert_both_engines("computed_const_after_for", src, "50\n");
}

#[test]
fn worker_fn_computed_const_after_while_loop() {
    let src = r#"
fn expensive(): number { return 42 }
let w = 0
while (w < 3) { w = w + 1 }
const K = expensive()
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    assert_both_engines("computed_const_after_while", src, "50\n");
}

#[test]
fn worker_fn_computed_const_after_expr_statement() {
    // A bare expression statement (`"ignored" + "me"`) before the const must not be
    // absorbed. (A pure expr keeps the program output to just the worker result.)
    let src = r#"
fn expensive(): number { return 42 }
"ignored" + "me"
const K = expensive()
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    assert_both_engines("computed_const_after_expr", src, "50\n");
}

#[test]
fn worker_fn_computed_const_after_if() {
    let src = r#"
fn expensive(): number { return 42 }
if (1 < 2) { let z = 99 }
const K = expensive()
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    assert_both_engines("computed_const_after_if", src, "50\n");
}

#[test]
fn worker_fn_computed_const_does_not_reship_absorbed_side_effect() {
    // `noisy()` is a bare call statement BEFORE the computed const. It must NOT be
    // absorbed into K's slice range — so `NOISY-RAN` prints EXACTLY ONCE (caller
    // side), not a second time when the isolate runs the slice.
    let src = r#"
fn noisy(): number { print("NOISY-RAN"); return 0 }
fn expensive(): number { return 42 }
noisy()
const K = expensive()
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    assert_both_engines("computed_const_no_reship", src, "NOISY-RAN\n50\n");
}

#[test]
fn aso_mode_worker_fn_computed_const_after_loop() {
    // The bounded-range fix must also survive build->run `.aso` (the slice is rebuilt
    // from stored `.aso` bytes, and `load_slice` does NOT run the verifier — so an
    // over-wide range would crash here too).
    let src = r#"
fn expensive(): number { return 42 }
for (i in 0..3) { i + 1 }
const K = expensive()
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    let (ok, out, err) = build_then_run_aso("computed_const_after_loop", src);
    assert!(ok, "[.aso run] failed: stdout={out:?} stderr={err:?}");
    assert_eq!(out, "50\n", "[.aso run] stdout mismatch (stderr={err:?})");
}

#[test]
fn worker_fn_computed_const_ternary_initializer() {
    // A computed-const whose OWN initializer is a top-level ternary (its condition is
    // consumed by an internal conditional jump, transiently emptying the stack). The
    // range must still cover the whole initializer, NOT cut off at the ternary arm.
    let src = r#"
const K = (5 > 3) ? 41 : 99
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(1))
}
await main()
"#;
    assert_both_engines("computed_const_ternary", src, "42\n");
}

#[test]
fn worker_fn_computed_const_short_circuit_initializer() {
    // A computed-const whose initializer is a short-circuit `&&` (also an internal
    // conditional jump).
    let src = r#"
fn truthy(): number { return 7 }
const K = truthy() && 42
worker fn g(n: number): number { return K + n }
async fn main() {
  print(await g(8))
}
await main()
"#;
    assert_both_engines("computed_const_short_circuit", src, "50\n");
}
