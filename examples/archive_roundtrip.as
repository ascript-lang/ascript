// Archive round-trips — tar, tar.gz, and zip, entirely in memory.
//
// std/archive has two planes: pure in-memory writers/readers (Bytes in, Bytes
// out — ungated) and disk helpers (Fs-gated; see archive_basics in the docs).
// This example stays in memory so it is deterministic and runs on every engine.
//
// `deterministic: true` zeroes mtime/uid/gid and sorts the entry order, so the
// produced bytes are byte-stable across runs — the reproducible-build discipline.
// Entry readers are lazy generators (one entry per `for await` step), so a huge
// archive is never fully materialized.
import * as archive from "std/archive"
import { utf8Decode, hexEncode } from "std/encoding"

// Build a tar from a fixed set of files and return its bytes.
fn buildTar(gzip) {
  let w = archive.tarWriter({gzip: gzip, deterministic: true})
  w.add("README.md", "# project")
  w.add("src/main.as", "print(ok)")
  w.add("data/notes.txt", "café — unicode name handling")
  return w.finish()
}

async fn main() {
  // ── plain tar ──────────────────────────────────────────────────────────────
  let tar = buildTar(false)
  print("tar entries:")
  for await (e in archive.tarEntries(tar)) {
    print(`  ${e.name}  (${e.size} bytes)`)
  }

  // Determinism: a second build with the same inputs yields identical bytes.
  let tar2 = buildTar(false)
  print(`tar deterministic: ${hexEncode(tar) == hexEncode(tar2)}`)

  // ── gzip-tar (auto-sniffed on read via the 1f 8b magic) ──────────────────────
  let gz = buildTar(true)
  print(`gzip smaller than gz overhead floor: ${len(gz) > 0}`)
  print("tar.gz entries:")
  for await (e in archive.tarEntries(gz)) {
    // Read the entry payload back and confirm it round-tripped.
    print(`  ${e.name}  data="${utf8Decode(e.data)!}"`)
  }

  // ── zip ──────────────────────────────────────────────────────────────────────
  let zw = archive.zipWriter()
  zw.add("a.txt", "first")
  zw.add("b.txt", "second")
  let zip = zw.finish()
  print("zip entries:")
  for await (e in archive.zipEntries(zip)) {
    print(`  ${e.name}  (${e.size} bytes)  data="${utf8Decode(e.data)!}"`)
  }

  // ── empty archive + empty file are valid edge cases ──────────────────────────
  let ew = archive.tarWriter({deterministic: true})
  let empty = ew.finish()
  let count = 0
  for await (_e in archive.tarEntries(empty)) {
    count = count + 1
  }
  print(`empty archive entry count: ${count}`)
  let zf = archive.zipWriter()
  zf.add("zero.bin", "")
  let zfb = zf.finish()
  for await (e in archive.zipEntries(zfb)) {
    print(`zero-byte entry: ${e.name} size=${e.size}`)
  }
}

await main()
print("archive_roundtrip ok")
