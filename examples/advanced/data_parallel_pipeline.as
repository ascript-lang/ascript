// data_parallel_pipeline.as — production-shaped data-parallel scoring pipeline
//
// Pattern: freeze a large read-only dataset once (zero-copy Arc hand-off to each
// chunk), `pmap` a scoring `worker fn` that returns `[value, err]` per element,
// partition successes/failures locally, `preduce` an aggregate over the successes,
// wrap the whole pipeline in `task.timeout` with a real fallback branch.
//
// No naked `!` — every Result pair is explicitly handled.
// Output is fully deterministic: frozen input, pinned {chunks: 4}, associative
// combiner, input-order merge.
//
//   ascript run examples/advanced/data_parallel_pipeline.as
import * as task from "std/task"
import * as shared from "std/shared"
import * as array from "std/array"

// ── helpers shipped to isolates via the code-slice closure ──────────────────

// Validate a row before scoring. Returns a [nil, err] pair on failure, or a
// [score, nil] pair on success. Top-level fn so the code slice ships it
// transitively alongside the worker entry.
fn validateAndScore(row) {
  if (row.value <= 0) {
    return [nil, {message: "non-positive value", id: row.id}]
  }
  if (row.weight <= 0) {
    return [nil, {message: "non-positive weight", id: row.id}]
  }
  // Weighted score: value * weight + small quality bonus.
  let score = row.value * row.weight + row.quality
  return [score, nil]
}

// ── worker entry: one `worker fn` per element ────────────────────────────────

// `score` is the pmap callback. It returns a [value, err] pair so failures
// appear as data elements — the caller partitions them after gather.
//
// Using `?`-propagation to surface validation errors:
//   validateAndScore returns [nil, err] → `?` propagates it out of the worker
//   body → the element result is the [nil, err] pair (not a chunk panic).
//
// This keeps every element accounted for in the output array and lets the
// caller decide what to do with failures — no information is lost.
worker fn score(row) {
  let pair = validateAndScore(row)
  pair? // propagate [nil,err] as the element; [score,nil] falls through
  return pair
}

// Associative integer combiner for preduce aggregate.
worker fn sumScores(a, b) {
  return a + b
}

// ── dataset generation ────────────────────────────────────────────────────────

// Build a deterministic dataset: 12 rows, two deliberately invalid (id 3 and 8).
fn buildDataset() {
  let rows = []
  let i = 0
  while (i < 12) {
    let value = i + 1
    // Row 2 (id 2): non-positive value triggers validation error.
    // Row 7 (id 7): non-positive weight triggers validation error.
    let weight = 2
    let v = value
    if (i == 7) {
      weight = -1
    }
    if (i == 2) {
      v = -5
    }
    array.push(rows, {id: i, value: v, weight: weight, quality: i % 3})
    i = i + 1
  }
  return rows
}

// ── pipeline ─────────────────────────────────────────────────────────────────
fn main() {
  // ── Step 1: freeze the dataset once ─────────────────────────────────────
  // shared.freeze builds an immutable Arc-backed graph. The frozen value
  // crosses to each chunk as a single Arc pointer bump — O(1) per chunk
  // regardless of dataset size, vs O(n) for an unfrozen copy.
  let dataset = buildDataset()
  let frozen = shared.freeze(dataset)

  // ── Step 2: pmap with timeout guard ─────────────────────────────────────
  // task.timeout returns a [value, err] pair: value on success, err on timeout.
  // pmap with {chunks: 4} is deterministic across core counts (input-order merge,
  // contractual chunk boundaries).
  let pipeline = task.pmap(frozen, score, {chunks: 4})
  let [scored, timeoutErr] = await task.timeout(10000, pipeline)
  if (timeoutErr != nil) {
    // Real fallback: report the timeout and surface a degraded result.
    print(`pipeline timed out after 10 s: ${timeoutErr.message}`)
    print("fallback: returning empty result set")
    print({successes: 0, failures: 0, total: 0})
    return
  }

  // ── Step 3: partition successes and failures ─────────────────────────────
  // Each element of `scored` is a [value, err] pair (worker fn returns them;
  // ?-propagation inside score() also yields them as element results).
  let successes = []
  let failures = []
  let i = 0
  while (i < len(scored)) {
    let [v, e] = scored[i]
    if (e != nil) {
      array.push(failures, {id: e.id, reason: e.message})
    } else {
      array.push(successes, v)
    }
    i = i + 1
  }
  print(`processed: ${len(scored)} rows`)
  print(`successes: ${len(successes)}, failures: ${len(failures)}`)

  // Print failure details.
  for (f of failures) {
    print(`  failed id=${f.id}: ${f.reason}`)
  }

  // ── Step 4: preduce aggregate over successes ─────────────────────────────
  // sumScores is associative (integer addition), so preduce equals sequential
  // reduce. Pin {chunks: 4} for identical output across different core counts.
  // init=0 is the additive identity; preduce([],…,0) would return 0 directly.
  let aggregate = await task.preduce(successes, sumScores, 0, {chunks: 4})
  print(`aggregate score: ${aggregate}`)

  // Sanity: verify against a local sequential sum.
  let localSum = 0
  for (s of successes) {
    localSum = localSum + s
  }
  print(`verified: ${aggregate == localSum}`)
}

await main()
