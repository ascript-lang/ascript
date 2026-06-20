// semver_ranges.as — SemVer 2.0.0 parsing, precedence, and node-style ranges.
//
// `std/semver` implements SemVer 2.0.0 precedence (including the prerelease
// rules) plus the node-semver range subset: caret `^`, tilde `~`, x-ranges,
// hyphen ranges, comparator sets (space = AND, `||` = OR). `parse`/`satisfies`/
// `maxSatisfying` are Tier-1 (versions/ranges are often external data); `compare`
// and `sort` Tier-2 on a malformed version (programmer-supplied).
import * as semver from "std/semver"

// parse → a structured version (Tier-1 [parsed, err]).
let [parsed, e] = semver.parse("1.4.2-rc.1+build.7")
print(`parse: ${parsed.major}.${parsed.minor}.${parsed.patch} pre=${parsed.prerelease} build=${parsed.build} (err: ${e})`)

// valid → bool.
print(`valid "1.0.0": ${semver.valid("1.0.0")}`)
print(`valid "1.0": ${semver.valid("1.0")}`)

// compare → -1 | 0 | 1 (full precedence, prerelease < release).
print(`compare 1.0.0 vs 2.0.0: ${semver.compare("1.0.0", "2.0.0")}`)
print(`compare 1.0.0 vs 1.0.0-rc.1: ${semver.compare("1.0.0", "1.0.0-rc.1")}`)
print(`compare 1.0.0-alpha vs 1.0.0-beta: ${semver.compare("1.0.0-alpha", "1.0.0-beta")}`)

// sort → ascending precedence order.
print(`sort: ${semver.sort(["1.10.0", "1.2.0", "1.2.0-rc.1", "1.1.0"])}`)

// satisfies → does a version match a range? (Tier-1 on a malformed range.)
fn check(v, range) {
  let [ok, err] = semver.satisfies(v, range)
  print(`  ${v} satisfies ${range}: ${ok}`)
}
print("range matching:")
check("1.4.2", "^1.2.0") // caret: >=1.2.0 <2.0.0
check("2.0.0", "^1.2.0") // out of range
check("1.2.9", "~1.2.3") // tilde: >=1.2.3 <1.3.0
check("1.3.0", "~1.2.3") // out of range
check("1.5.0", "1.x") // x-range
check("1.2.7", "1.2.3 - 1.5.0") // hyphen range
check("0.2.5", "^0.2.0") // caret 0.x special case: >=0.2.0 <0.3.0
check("3.0.0", "1.x || 3.x") // OR of comparator sets

// A prerelease only satisfies a range when a comparator shares its
// [major,minor,patch] tuple AND carries a prerelease (node's default).
check("1.2.3-rc.1", ">=1.2.3-rc.0 <1.3.0")
check("1.2.3-rc.1", "^1.0.0") // false: no prerelease in the matching comparator

// maxSatisfying → the highest candidate in range (Tier-1).
let [best, _be] = semver.maxSatisfying(["1.1.0", "1.4.2", "1.9.0", "2.1.0"], "^1.2.0")
print(`maxSatisfying ^1.2.0: ${best}`)

print("semver_ranges ok")
