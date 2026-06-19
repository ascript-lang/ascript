// backup_tool.as — walk a directory, build a DETERMINISTIC tar.gz, verify it.
//
// archive.tarCreateFromDir(dir, {gzip, deterministic}) is the Fs-gated convenience
// for packaging a tree. With `deterministic: true` it sorts the walk and zeroes
// mtime/uid/gid, so the SAME tree always produces byte-identical archive bytes —
// the reproducible-backup discipline (two runs → an identical artifact you can
// hash-compare).
//
// This example builds a small fixed tree under a unique temp directory (so
// parallel runs never collide), archives it twice to prove byte-stability, reads
// the entries back to verify, and cleans up. It prints only the RELATIVE entry
// names + sizes (deterministic) — never the absolute temp path.
import * as fs from "std/fs"
import * as os from "std/os"
import * as uuid from "std/uuid"
import * as archive from "std/archive"
import * as array from "std/array"
import { hexEncode, utf8Decode } from "std/encoding"

// Lay down a fixed source tree under `base`. Returns the file count written.
fn seedTree(base) {
  fs.mkdir(fs.join(base, "src"), {recursive: true})
  fs.mkdir(fs.join(base, "docs"), {recursive: true})
  fs.write(fs.join(base, "README.md"), "# my project\nA reproducible backup demo.")
  fs.write(fs.join(base, "src/main.as"), "fn main() { print(\"hi\") }\nmain()")
  fs.write(fs.join(base, "src/util.as"), "fn helper(x) { return x * 2 }")
  fs.write(fs.join(base, "docs/guide.md"), "## Guide\nUse café-safe unicode.")
  return 4
}

async fn main() {
  // A unique base dir (random suffix, never printed) keeps concurrent runs
  // isolated — the output stays deterministic, only the temp path varies.
  let base = fs.join(os.tempDir(), `ascript-backup-${uuid.v4()}`)
  let [_m, mkErr] = fs.mkdir(base, {recursive: true})
  if (mkErr != nil) {
    print(`mkdir error: ${mkErr.message}`)
    return
  }
  let written = seedTree(base)
  print(`seeded ${written} files`)

  // Build the backup archive — deterministic gzip-tar.
  let [tgz1, e1] = archive.tarCreateFromDir(base, {gzip: true, deterministic: true})
  if (e1 != nil) {
    print(`archive error: ${e1.message}`)
    fs.remove(base, {recursive: true})
    return
  }

  // Build it a SECOND time: deterministic mode → byte-identical artifact.
  let [tgz2, e2] = archive.tarCreateFromDir(base, {gzip: true, deterministic: true})
  print(`reproducible: ${e2 == nil && hexEncode(tgz1) == hexEncode(tgz2)}`)

  // Verify by reading the entries back (gzip is auto-sniffed on read).
  let names = []
  let fileCount = 0
  for await (e in archive.tarEntries(tgz1)) {
    names = [...names, `${e.name} (${e.size}B)`]
    if (e.size > 0) {
      fileCount = fileCount + 1
    }
  }
  print("archive contents:")
  for (n of array.sort(names)) {
    print(`  ${n}`)
  }

  // Confirm a known file round-tripped with the right bytes.
  let readmeBody = nil
  for await (e in archive.tarEntries(tgz1)) {
    if (e.name == "README.md") {
      readmeBody = utf8Decode(e.data)!
    }
  }
  print(`README round-trips: ${readmeBody == "# my project\nA reproducible backup demo."}`)
  print(`non-empty entries: ${fileCount}`)

  // Clean up the temp tree.
  fs.remove(base, {recursive: true})
}

await main()
print("backup_tool ok")
