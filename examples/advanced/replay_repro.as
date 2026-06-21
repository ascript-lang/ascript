// Record a failure, replay it offline — the canonical record/replay workflow.
//
// THE STORY: a data pipeline reads a config off disk, runs a subprocess to derive
// a fact, and validates the result. In production it failed. Instead of guessing,
// you RECORD the run:
//
//   ascript run --record crash.astrc examples/advanced/replay_repro.as
//
// Now `crash.astrc` is a portable, deterministic capture of every effect the run
// observed — the config bytes, the subprocess output, every clock and random draw.
// Hand it to a teammate, attach it to a bug report, or carry it onto a plane and
// REPLAY it with NO real I/O — the disk fixture can be gone, the network can be
// down, the subprocess binary can be uninstalled, and replay still reproduces the
// exact run:
//
//   ascript run --replay crash.astrc examples/advanced/replay_repro.as
//
// Then debug it with TIME TRAVEL — set a breakpoint at the failure and step
// BACKWARDS to the corruption point, every effect pinned:
//
//   ascript dap --replay crash.astrc          # editor-driven
//   ascript run --inspect --replay crash.astrc examples/advanced/replay_repro.as
//
// FULL ERROR HANDLING: every effect is a Tier-1 `[value, err]` pair handled
// explicitly (a recoverable result, never a crash), so the pipeline degrades
// gracefully whether or not the fixture/subprocess is present. The printed output
// is DETERMINISTIC across runs (derived facts, not raw effect bytes) so this file
// is four-mode byte-identity tested as an ordinary corpus program.
import * as fs from "std/fs"
import * as process from "std/process"

// Stage 1 — read a config off disk (RECORDED). Under --replay the recorded bytes
// come back with no real fs access. We read this source file (always present in a
// fresh checkout) so the standalone run is deterministic; the recovery branch
// proves a missing fixture is recoverable DATA, not a panic.
fn loadConfig() {
  let [content, err] = fs.read("examples/advanced/replay_repro.as")
  if (err != nil) {
    // The fixture is gone — a recoverable miss, surfaced as a typed result.
    return [nil, err]
  }
  return [content, nil]
}

// Stage 2 — run a subprocess to derive a fact (RECORDED). `process.run` waits and
// captures stdout; under --replay the captured `{stdout, code, success}` comes back
// with no real subprocess. `echo` is portable and its output is deterministic.
fn deriveToken() {
  let [result, err] = await process.run("echo", ["pipeline-ok"])
  if (err != nil) {
    // Spawn failure (binary missing / denied) — recoverable, not a crash.
    return [nil, err]
  }
  return [result, nil]
}

fn main() {
  // Stage 1: load + validate the config.
  let [config, cfgErr] = loadConfig()
  if (cfgErr != nil) {
    // Offline / missing-fixture path: report the recoverable failure deterministically.
    print(`config load failed (recoverable): ${cfgErr.message != nil}`)
    print("replay this run with: ascript run --replay crash.astrc <file>")
    return
  }
  // Derived fact, not the raw bytes: the config loaded and is non-empty.
  print(`config non-empty: ${len(config) > 0}`) // config non-empty: true

  // Stage 2: derive the token via the subprocess.
  let [proc, procErr] = deriveToken()
  if (procErr != nil) {
    // Subprocess unavailable: still recoverable, still deterministic to print.
    print(`subprocess failed (recoverable): ${procErr.message != nil}`)
    return
  }
  // Derived facts about the subprocess result — never the raw stdout bytes verbatim
  // beyond the known echoed token (which IS deterministic for `echo`).
  let token = proc.stdout
  print(`subprocess succeeded: ${proc.success}`) // true
  print(`token is the echoed line: ${token == "pipeline-ok\n"}`) // true

  // Stage 3: the "validation" the production run failed at. Here it passes; the
  // point of the example is that a FAILING version of this check is exactly what
  // you would record and step backwards through to find the corruption point.
  let valid = len(config) > 0 && proc.success && len(token) > 0
  print(`pipeline valid: ${valid}`) // pipeline valid: true
  print("done — record: ascript run --record crash.astrc <file>")
}

await main()
