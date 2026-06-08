// Typed errors via a payload-carrying enum. AScript models a fallible result as a
// `[value, err]` pair; with algebraic enums the error slot becomes a RICH, typed sum
// (`DbError.NotFound(key)`) instead of a bare string. The `?` / `!` operators are
// unchanged — they inspect the pair SHAPE, not the error's kind — so an enum error
// rides the `err` slot as ordinary data, and the caller `match`es it EXHAUSTIVELY:
// every error case must be handled or it is a compile error.
import * as map from "std/map"

// The closed set of failures this layer can produce. Each variant carries the
// context a handler needs: the missing key, the elapsed time, the failure detail.
enum DbError {
  NotFound(key: string),
  Timeout(ms: int),
  Conn(detail: string),
}

// A fallible lookup returns the `[value, err]` pair convention: `[value, nil]` on
// success, `[nil, DbError.…]` on failure. (No tuple return annotation — the pair is
// the idiom, and leaving it inferred keeps the gradual checker silent.)
fn lookup(store, k: string) {
  if (!map.has(store, k)) {
    return [nil, DbError.NotFound(k)]
  }
  return [map.get(store, k), nil]
}

// `?` propagates the failure pair OUT of this function untouched: on a hit it binds
// the value, on a miss it early-returns the same `[nil, DbError]` to OUR caller.
fn greet(store, k: string) {
  let name = lookup(store, k)?
  return [`hello, ${name}`, nil]
}

// Turn a typed error into a message — an EXHAUSTIVE match over `DbError`. Adding a
// fourth variant to `DbError` would make this a `non-exhaustive-match` COMPILE error
// until the new case is handled. No `_` catch-all: every case is named.
fn explain(e: DbError): string {
  return match e {
    NotFound(key) => `no such key: ${key}`,
    Timeout(ms) => `timed out after ${ms}ms`,
    Conn(detail) => `connection failed: ${detail}`,
  }
}

let store = map.new()
map.set(store, "ada", "Ada Lovelace")

// Success path: `?` unwraps the value, `greet` returns a value pair.
let ok = greet(store, "ada")
print(ok[0]) // hello, Ada Lovelace

// Failure path: the `NotFound` error propagated out of `greet` via `?`, and the
// caller dispatches on it.
let bad = greet(store, "nobody")
print(bad[0]) // nil
print(explain(bad[1])) // no such key: nobody

// `!` force-unwraps a present value (the value, or a recoverable panic on a failure
// pair). Here the lookup succeeds, so `!` yields the value directly.
let direct = lookup(store, "ada")!
print(direct) // Ada Lovelace

// The other error variants, each explained through the same exhaustive match.
print(explain(DbError.Timeout(500))) // timed out after 500ms
print(explain(DbError.Conn("refused"))) // connection failed: refused

// Errors are ordinary structural values: two equal-payload errors compare equal,
// and the payload is reflectable.
print(DbError.NotFound("x") == DbError.NotFound("x")) // true
print(DbError.Timeout(500).name) // Timeout
print(DbError.Timeout(500).value) // {ms: 500}
