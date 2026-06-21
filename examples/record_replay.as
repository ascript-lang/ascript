// Record/replay — capture a run's non-deterministic inputs and effectful results
// into a portable trace, then re-execute byte-identically with NO real I/O.
//
//   ascript run --record run.astrc examples/record_replay.as   # capture a trace
//   ascript run --replay run.astrc examples/record_replay.as   # re-run from it
//
// THE DETERMINISTIC-MODE CONTRACT (active under --record AND --replay):
//   * the wall/monotonic clock becomes a VIRTUAL clock (no real time read),
//   * `time.sleep` does NOT sleep — it advances the virtual clock INSTANTLY,
//   * `math.random`/`uuid.v4`/`crypto.randomBytes` draw from a SEEDED PRNG
//     (pin the seed with `--seed N`; default is OS entropy, stored in the trace).
// So a recorded run already differs from a plain run in pacing and random values
// (both still valid) — and that is exactly why REPLAY is fast: replay performs no
// network/disk I/O, sleeps are instant, and recorded results return inline.
//
// WHAT GETS RECORDED: clock/RNG/UUID are SEAMED (replayed from the seed); fs/env/
// `process.run`/DNS/buffered `http`/`workflow.run` are RECORDED at the result
// boundary (replay returns the captured value with no real I/O — delete the file
// or unplug the network and replay still matches). Sockets/servers/streams/workers
// have no determinism seam in v1 and are a LOUD refusal at RECORD, so a trace that
// records successfully always replays.
//
// This file runs standalone with NO flags as an ordinary program, so its printed
// output must be DETERMINISTIC across runs (it is four-mode byte-identity tested).
// It therefore prints DERIVED FACTS about the effects (always-true invariants),
// never the raw random/time/path-dependent bytes — the VALUE here is the demo of
// which effects are seamed/recorded, explained above and below.
import * as math from "std/math"
import * as uuid from "std/uuid"
import * as time from "std/time"
import * as env from "std/env"
import * as fs from "std/fs"

fn main() {
  // Seamed: RNG. Under a trace these draw from the seeded PRNG and replay
  // identically. Standalone we print the always-true RANGE invariant, not `r`.
  let r = math.random()
  print(`random in [0,1): ${r >= 0.0 && r < 1.0}`) // random in [0,1): true

  // Seamed: UUID. A v4 UUID is always 36 chars (8-4-4-4-12 + hyphens).
  let id = uuid.v4()
  print(`uuid is 36 chars: ${len(id) == 36}`) // uuid is 36 chars: true

  // Seamed: the clock. `time.now()` is virtual under a trace; standalone it is the
  // real epoch — either way it is a positive millisecond count.
  print(`clock is positive: ${time.now() > 0}`) // clock is positive: true

  // Seamed: instant sleep. Under a trace `time.sleep` advances the virtual clock
  // without delaying; standalone it is a real (tiny) sleep. The observable fact —
  // that monotonic time does not go backwards across it — holds in both.
  let t0 = time.monotonic()
  time.sleep(1)
  let t1 = time.monotonic()
  print(`time moved forward: ${t1 >= t0}`) // time moved forward: true

  // Recorded: env. `env.get` returns the captured value at replay (with no real
  // environment read). `PATH` may or may not be set, so we print only the shape of
  // the result: a string or nil — deterministic either way.
  let path = env.get("PATH")
  print(`PATH is string or nil: ${path == nil || len(path) >= 0}`) // ... : true

  // Recorded: fs. `fs.read` returns a Tier-1 `[content, err]` pair; under --replay
  // the recorded body comes back even if the file no longer exists. We read THIS
  // source file (always present standalone) and print only whether the read
  // succeeded and is non-empty — not the file body (which would be path- and
  // checkout-dependent).
  let [content, err] = fs.read("examples/record_replay.as")
  if (err == nil) {
    print(`read this file, non-empty: ${len(content) > 0}`) // ... : true
  } else {
    // Recoverable: run from a different CWD and the read fails as DATA, not a crash.
    print(`read failed (recoverable): ${err.message != nil}`) // ... : true
  }
  print("done — try: ascript run --record run.astrc examples/record_replay.as")
}

main()
