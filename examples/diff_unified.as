// diff_unified.as — line/char diffs and the unified (patch) format.
//
// `std/diff` is a hand-rolled Myers O(ND) diff. `diff.unified` emits the
// familiar `--- / +++ / @@` patch text (the same format `git diff` and `patch`
// speak); `diff.lines`/`diff.chars` return structured hunks. Output is fully
// deterministic. Non-string args are a Tier-2 panic; oversized inputs return a
// Tier-1 "inputs too large".
import * as diff from "std/diff"

let before = "alpha\nbeta\ngamma\ndelta\nepsilon\n"
let after = "alpha\nBETA\ngamma\ndelta\nzeta\nepsilon\n"

// unified: a ready-to-read patch (default 3 lines of context).
print("--- unified (default context) ---")
print(diff.unified(before, after, {fromFile: "before.txt", toFile: "after.txt"}))

// Tighter context merges nearby changes differently.
print("--- unified (context: 1) ---")
print(diff.unified(before, after, {context: 1, fromFile: "before.txt", toFile: "after.txt"}))

// The "\ No newline at end of file" marker is emitted correctly when an input
// lacks a trailing newline.
print("--- no trailing newline ---")
print(diff.unified("one\ntwo", "one\ntwo\nthree\n", {fromFile: "a", toFile: "b"}))

// diff.lines: structured hunks, each tagged equal / delete / insert.
print("--- diff.lines hunks ---")
for (h of diff.lines(before, after)) {
  print(`  ${h.tag}: a[${h.aStart}..${h.aEnd}] b[${h.bStart}..${h.bEnd}] -> ${h.lines}`)
}

// diff.chars: intra-line character-level diff (for small inputs).
print("--- diff.chars hunks ---")
for (h of diff.chars("color", "colour")) {
  print(`  ${h.tag}: ${h.lines}`)
}

print("diff_unified ok")
