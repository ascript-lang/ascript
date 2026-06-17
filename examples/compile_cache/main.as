// ─────────────────────────────────────────────────────────────────────────────
// Compile cache demo (WARM A) — a small multi-module program.
//
//   main.as  ──imports──▶  util.as  ──imports──▶  model.as
//
// `ascript run` compiles the WHOLE import graph (parse + resolve + bytecode) on
// every invocation. WARM A adds a content-addressed COMPILE CACHE so the second
// run of an unchanged program skips all of that and runs the cached, verified
// bytecode directly:
//
//   ascript run examples/compile_cache/main.as     # cold: compile + cache
//   ascript run examples/compile_cache/main.as     # warm: cache HIT, no compile
//
// The cache is keyed airtight: edit ANY module in the graph (this file, util.as,
// or model.as) and the next run MISSES and recompiles — a stale hit would run
// wrong code, so the cache re-hashes every reachable source file's content on
// every lookup (mtime touches don't matter, only content). The cache is fully
// transparent: cached and uncached runs are byte-identical (stdout, stderr, and
// panic carets alike).
//
// Bypass it with `--no-cache` or `ASCRIPT_NO_COMPILE_CACHE=1`; inspect/clean it
// with `ascript cache dir` / `ascript cache clean`. The cache is local-only and
// never applies to `--tree-walker`, `--inspect`, `--profile`, or `run file.aso`.
// ─────────────────────────────────────────────────────────────────────────────

import { run, run_loud } from "./util"

fn main() {
  print(run("world"))        // hello world
  print(run_loud("cache"))   // hello cache!
}

main()
