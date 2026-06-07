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
