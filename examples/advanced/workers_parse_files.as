// Parallel document processing: parse/transform N in-memory "documents" in
// worker isolates, gather in order, report deterministic results.
//
// The documents are plain objects (sendable across isolate boundaries).
// The "parsing" step (wordCount + validation) is a pure-language helper fn
// that the code-slice builder ships transitively along with the worker entry.
//
// NOTE: stdlib imports (e.g. `json.parse`) are not available inside a worker
// fn body — the code slice only ships top-level fn/const defs, not import
// bindings. Use pure top-level helper fns for any in-worker computation.
import * as task from "std/task"
import * as array from "std/array"

// Count whitespace-delimited words in a string. Pure language: iterates over
// characters with a for-in loop; no stdlib imports required.
fn wordCount(s: string): number {
  let count = 0
  let inWord = false
  for (ch in s) {
    if (ch == " " || ch == "\t" || ch == "\n") {
      inWord = false
    } else if (!inWord) {
      count = count + 1
      inWord = true
    }
  }
  return count
}

// Validate and transform a document object into a summary record.
// Returns a [result, err] pair — the canonical Tier-1 Result pattern.
fn parseDocument(doc) {
  if (doc.title == nil || len(doc.title) == 0) {
    return [nil, Err("document missing title")[1]]
  }
  if (doc.body == nil) {
    return [nil, Err("document missing body")[1]]
  }
  return [{ id: doc.id, title: doc.title, words: wordCount(doc.body) }, nil]
}

// Ship `parseDocument` and `wordCount` to a worker isolate. Both are
// top-level fns so the code-slice builder includes them transitively.
worker fn processDoc(doc) {
  return parseDocument(doc)
}

fn main() {
  // In-memory "documents" — no real filesystem I/O; fully hermetic.
  let docs = [
    { id: 1, title: "intro",  body: "the quick brown fox jumps over the lazy dog" },
    { id: 2, title: "middle", body: "pack my box with five dozen liquor jugs" },
    { id: 3, title: "end",    body: "how vexingly quick daft zebras jump" },
  ]

  // Dispatch each document to a worker isolate, gather results in input order.
  let futures = array.map(docs, processDoc)
  let results = await task.gather(futures)

  // Unwrap each [result, err] pair; panic on any error (none expected here).
  let parsed = []
  let i = 0
  while (i < len(results)) {
    let [rec, err] = results[i]
    if (err != nil) { assert(false, err.message) }
    array.push(parsed, rec)
    i = i + 1
  }

  // Print summaries in fixed (input) order.
  let j = 0
  while (j < len(parsed)) {
    let r = parsed[j]
    print(`doc ${r.id} (${r.title}): ${r.words} words`)
    j = j + 1
  }
}

await main()
