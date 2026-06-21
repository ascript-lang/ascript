#!/usr/bin/env node
// WASM §5.6 / Task 2.2 — the wasm↔native mini-differential smoke.
//
// Not a fifth differential MODE — a corpus smoke that proves the wasm build produces
// BYTE-IDENTICAL captured output to `target/release/ascript run <file>` on native for a
// curated subset of `examples/*.as`. Same VM, same compiler → output must match exactly;
// any divergence is a real cross-platform bug (most likely a clock/RNG/log-routing leak —
// those examples are EXCLUDED below with a recorded reason, never silently skipped).
//
// ISOLATION: each example runs in a FRESH wasm instance (a child `node --eval` per program).
// This mirrors the real playground's per-run `worker.terminate()` + lazy re-instantiate
// (§5.5): gcmodule 0.3 leaks dead *cycles* on wasm (the per-isolate `collect_thread_cycles`
// is skipped — see src/lib.rs wasm_run_source), so cycles accumulate within one wasm
// instance; running ~40 programs in ONE instance eventually corrupts the heap. One fresh
// instance per program keeps each run clean, exactly as the browser playground does.
//
// Usage: node scripts/wasm_smoke.mjs <path-to-native-ascript-bin>
//   (the built pkg is read from ascript-wasm/pkg/ — run scripts/build-wasm.sh first.)
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..");
const nativeBin = process.argv[2];
if (!nativeBin) {
  console.error("usage: node scripts/wasm_smoke.mjs <native ascript binary>");
  process.exit(2);
}
const pkgDir = join(repo, "ascript-wasm", "pkg");

// ── EXAMPLES_WASM ─────────────────────────────────────────────────────────────────
// The committed include list (WASM §5.6). Derived by sweeping examples/*.as: included
// iff (top-level std imports ∩ excluded-modules = ∅) AND no worker/timer-resource use AND
// it is a single self-contained source (no relative sibling imports — no fs in the
// playground). The allowed module set is the wasm feature subset: CORE (array/string/math/
// object/map/set/convert/task/sync/stream/time/schema/caps/assert/bench/events/lru/
// template/decimal) + data (json/regex/encoding/csv/toml/yaml/uuid/url/bytes) + binary
// (msgpack/cbor) + log + shared. Shrinking this list later requires a recorded reason here.
const EXAMPLES_WASM = [
  "async",
  "data",
  "default_params",
  "defer",
  "enums_adt",
  "enums_negative_backing",
  "events_emitter",
  "factorial",
  "force_unwrap",
  "functions",
  "generators",
  "generics",
  "hello",
  "instanceof",
  "interfaces",
  "lru_cache",
  "map_literals",
  "match_or_patterns",
  "num_int_float_edges",
  "numbers",
  "object_destructuring",
  "object_order_stress",
  "oop",
  "optional_types",
  "pattern_matching",
  "range_step_default",
  "ranges",
  "records",
  "regex",
  "rest",
  "result",
  "schema_collect",
  "shape_validation",
  "spread",
  "static_methods",
  "strings",
  "template_render",
  "typed",
  "typed_contracts",
  "typed_fields",
  "typed_parse",
];

// Recorded EXCLUSIONS (allowed-module examples deliberately left out — each with a reason;
// removing an exclusion is fine, adding one needs a reason here, per §5.6).
const EXCLUDED = {
  // Different recursion ceiling on wasm (MAX_CALL_DEPTH 1000 vs native 3000) → the program
  // is calibrated to the native depth and would diverge by construction (§5.3.5). Covered
  // instead by the node_smoke recursion tests against the wasm ceiling.
  deep_recursion: "wasm MAX_CALL_DEPTH (1000) differs from native (3000) — calibrated apart",
  // Multi-module examples: they `import` a SIBLING `.as` file by relative path, which needs
  // filesystem module resolution. The playground runs a single source STRING with no fs (and
  // `std/fs` is denied anyway), so relative-import programs cannot resolve their siblings.
  bundle_multimodule: "imports a sibling .as by relative path — needs fs module resolution",
  bundle_util: "the imported helper module of bundle_multimodule — not a standalone program",
  // `std/log` routes to stderr (Live) on native but into the CAPTURE buffer on wasm; the
  // mini-differential compares wasm capture vs native STDOUT, so a log-using program shows
  // the log lines on wasm but not in native stdout — an expected routing difference, not a
  // VM bug. (node_smoke covers the wasm path directly.)
  logging: "std/log goes to native stderr but wasm capture — stdout compare would diverge",
};

// The per-example child driver: load the bindgen pkg in a FRESH process, run ONE program,
// print the JSON RunResult to stdout. The AScript source is passed via an ENV var (never
// interpolated into the script string), so arbitrary source content can't break the driver
// or inject anything; the script itself is a fixed argument-array element to `node` (no
// shell). Paths are baked in as JSON literals (trusted, repo-local).
const CHILD = `
import { readFileSync } from "node:fs";
const mod = await import(${JSON.stringify(join(pkgDir, "ascript_wasm.js"))});
await mod.default(readFileSync(${JSON.stringify(join(pkgDir, "ascript_wasm_bg.wasm"))}));
const res = await mod.run_program(process.env.ASCRIPT_WASM_SRC);
process.stdout.write(JSON.stringify(res));
`;

function runWasm(source) {
  const out = execFileSync("node", ["--input-type=module", "--eval", CHILD], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"],
    maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, ASCRIPT_WASM_SRC: source },
  });
  return JSON.parse(out);
}

function nativeOutput(file) {
  return execFileSync(nativeBin, ["run", file], { encoding: "utf8" });
}

const allowEsc = /\x1b/; // ANSI escape (ESC U+001B) — must never appear in wasm output

let failures = 0;
let checked = 0;
for (const name of EXAMPLES_WASM) {
  const file = join(repo, "examples", `${name}.as`);
  const source = readFileSync(file, "utf8");

  const native = nativeOutput(file);
  let res;
  try {
    res = runWasm(source);
  } catch (e) {
    failures++;
    checked++;
    console.error(`✗ ${name}: wasm child process crashed`);
    continue;
  }
  checked++;

  if (!res.ok) {
    failures++;
    console.error(`✗ ${name}: wasm run failed (ok=false)`);
    if (res.error) console.error(`    error: ${res.error.split("\n")[0]}`);
    continue;
  }
  if (allowEsc.test(res.output)) {
    failures++;
    console.error(`✗ ${name}: wasm output contains an ANSI escape`);
    continue;
  }
  if (res.output !== native) {
    failures++;
    console.error(`✗ ${name}: wasm output != native output`);
    const w = res.output.split("\n");
    const n = native.split("\n");
    const max = Math.max(w.length, n.length);
    for (let i = 0; i < max; i++) {
      if (w[i] !== n[i]) {
        console.error(`    line ${i + 1}:`);
        console.error(`      native: ${JSON.stringify(n[i])}`);
        console.error(`      wasm:   ${JSON.stringify(w[i])}`);
      }
    }
    continue;
  }
  console.log(`✓ ${name}`);
}

console.log(
  `\nwasm_smoke: ${checked - failures}/${checked} byte-equal ` +
    `(${Object.keys(EXCLUDED).length} recorded exclusions)`
);
if (failures > 0) process.exit(1);
