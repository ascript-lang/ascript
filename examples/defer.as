// `defer [await] <call>` registers a call to run when the enclosing function
// exits — by any route: normal completion, `return`, `?`-propagation, or panic.
// Deferred calls execute LIFO (last-registered, first-run), and the callee +
// arguments are evaluated at the `defer` statement, not at function exit.
//
// §1  Basic resource-close pattern
// §2  LIFO ordering
// §3  Argument snapshot — value captured at `defer` time
// §4  `?`-propagation interplay (defers still run)
// §5  `defer await` for async cleanup
// §6  Function-scoped rule (defer inside an `if`/block)
// §7  Per-iteration cleanup via a wrapper function (the correct loop idiom)

// ---------------------------------------------------------------------------
// §1  Basic resource-close pattern
//
// A small class with an explicit `close()` method illustrates the idiom that
// defer was built for: guarantee cleanup on EVERY exit, including early returns.
// ---------------------------------------------------------------------------
class Handle {
  fn init(name) {
    self.name = name
    self.open = true
  }
  fn close() {
    self.open = false
    print(`closed: ${self.name}`)
  }
}

fn useHandle(name) {
  let h = Handle(name)
  defer h.close() // runs on every exit below — normal, return, or panic
  print(`using: ${h.name}`)
}

useHandle("db") // using: db
// closed: db

// ---------------------------------------------------------------------------
// §2  LIFO ordering
//
// Multiple defers on the same function execute newest-first (LIFO). Registering
// three defers in order A→B→C means they run in order C→B→A.
// ---------------------------------------------------------------------------
fn lifoDemo() {
  defer print("first registered") // runs last
  defer print("second registered") // runs middle
  defer print("third registered") // runs first
  print("body")
}

lifoDemo()
// body
// third registered
// second registered
// first registered

// ---------------------------------------------------------------------------
// §3  Argument snapshot — value captured at `defer` time
//
// The arguments to a deferred call are evaluated WHEN the `defer` statement
// executes, not when the function exits. Mutating a local after deferring it
// does NOT change what the defer prints.
//
// Note: `defer print(x)` captures the VALUE of `x` at the `defer` statement.
// Contrast with `defer (() => print(x))()` — a closure captures a mutable
// binding as a shared cell and WOULD see the later mutation.
// ---------------------------------------------------------------------------
fn snapshotDemo() {
  let x = 1
  defer print(x) // evaluates x → 1 NOW; mutation below is invisible
  x = 42
  print(`x is now ${x}`)
}

snapshotDemo()
// x is now 42
// 1

// ---------------------------------------------------------------------------
// §4  `?`-propagation interplay
//
// A function that uses `?` to propagate an error still drains its defer stack
// before the error reaches the caller. This is the key gap `defer` closes:
// without it, every `?` early-exit would skip the manual cleanup below it.
// ---------------------------------------------------------------------------
fn fallible(shouldFail) {
  defer print("cleanup: runs even when ? propagates")
  if (shouldFail) {
    return Err("something went wrong")
  }
  return Ok("ok")
}

fn caller() {
  // `?` propagates the error pair; the defer in `fallible` still fires.
  let v = fallible(true)?
  return Ok(v)
}

let bad = caller()
print(bad[1].message) // something went wrong
let good = fallible(false)
print(good[0]) // ok

// ---------------------------------------------------------------------------
// §5  `defer await` for async cleanup
//
// When cleanup is itself an async function, use `defer await f()` — the
// statement-level `await` drives the future to completion before the next
// (older) defer runs, keeping the LIFO sequence intact. A bare `defer f()` on
// an async fn is a runtime error (it would silently cancel the cleanup future).
// ---------------------------------------------------------------------------
async fn asyncClose(name) {
  print(`async close: ${name}`)
}

async fn withAsyncCleanup() {
  defer await asyncClose("second") // registered first → runs second
  defer await asyncClose("first") // registered second → runs first
  print("async body")
}

await withAsyncCleanup()
// async body
// async close: first
// async close: second

// ---------------------------------------------------------------------------
// §6  Function-scoped rule
//
// A `defer` inside an `if` or any nested block still belongs to the FUNCTION
// activation, not the block. It runs when the function exits, not when the
// block exits. This matches Go semantics.
// ---------------------------------------------------------------------------
fn functionScopedRule(cond) {
  if (cond) {
    defer print("deferred inside if — runs at FUNCTION exit")
  }
  print("after if block")
}

functionScopedRule(true)
// after if block
// deferred inside if — runs at FUNCTION exit

// ---------------------------------------------------------------------------
// §7  Per-iteration cleanup via a wrapper function
//
// A bare `defer` inside a loop body accumulates one entry PER ITERATION — all
// run at function exit (LIFO). The `defer-in-loop` lint warns about this
// because it is usually unintentional. The CORRECT idiom when you want
// per-iteration cleanup is to extract the body into a helper function and let
// THAT function own the defer. Each call to the helper gets its own defer
// stack, so cleanup fires per-call at helper exit — exactly one cleanup per
// iteration, in the right place.
//
// GATE-5 NOTE: we use option (a) here — the wrapper function pattern — so
// `ascript check examples/defer.as` stays clean (zero diagnostics). This also
// teaches the right idiom, not just the footgun.
// ---------------------------------------------------------------------------
fn processItem(item) {
  defer print(`  cleanup: ${item}`) // per-call defer, fires at helper exit
  print(`  process: ${item}`)
}

fn processAll(items) {
  for (item of items) {
    processItem(item) // each call gets its own defer stack
  }
}

print("--- loop demo ---")
processAll(["a", "b", "c"])
//   process: a
//   cleanup: a
//   process: b
//   cleanup: b
//   process: c
//   cleanup: c
print("defer ok")
