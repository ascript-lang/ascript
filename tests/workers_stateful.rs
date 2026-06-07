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
