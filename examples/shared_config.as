// shared_config.as — std/shared: an immutable, zero-copy-across-isolates config.
//
// `shared.freeze(v)` deep-converts a value into an immutable, Arc-backed graph and
// returns a Value::Shared — AScript's FIRST Send value. A frozen value READS exactly
// like the value it froze (scalar / descend / index / method / iterate), only it is
// immutable and shareable across worker isolates by an Arc pointer bump (no copy).
// Any write to it is a recoverable Tier-2 panic ("cannot mutate a frozen {kind}").
//
//   ascript run examples/shared_config.as
import * as shared from "std/shared"
import * as json from "std/json"

fn main() {
  // Build the config once and freeze it. The result is an immutable DAG.
  let cfg = shared.freeze({region: "us-east-1", flags: {beta: true, canary: false}, limits: [10, 100, 1000]})

  // Reads look exactly like reads of the original object.
  print(cfg.region) // scalar read (materialized)        -> us-east-1
  print(cfg.flags.beta) // descend (Shared view) -> scalar   -> true
  print(cfg.limits[0]) // index a frozen array              -> 10
  print(cfg.limits.len()) // read-only method                  -> 3
  print(cfg.has("region")) // membership                        -> true

  // Iteration over a frozen array is zero-copy.
  let total = 0
  for (l of cfg.limits) {
    total = total + l
  }
  print(total) // 1110

  // freeze is idempotent: freezing an already-frozen value returns the SAME value.
  print(shared.freeze(cfg) == cfg) // true
  print(shared.isShared(cfg)) // true

  // A write is rejected — the value is frozen. recover() catches the Tier-2 panic.
  let [_, e1] = recover(() => cfg.region = "eu")
  print(e1.message) // cannot mutate a frozen object
  let [__, e2] = recover(() => cfg.limits.push(9999))
  print(e2.message) // cannot mutate a frozen array

  // A frozen value serializes exactly like its underlying kind — the headline
  // "freeze the config once, emit it on every request" path. And a frozen value
  // nested inside a LIVE object does NOT poison the enclosing serialization.
  print(json.stringify(cfg)[0]) // {"region":"us-east-1","flags":{...},"limits":[...]}
  print(json.stringify({snapshot: cfg, ok: true})[0]) // frozen child, live parent
}

main()
