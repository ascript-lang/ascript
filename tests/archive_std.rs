//! BATT B1 §6 — end-to-end `std/archive` script-level tests (streaming tar).
//!
//! The `src/stdlib/archive.rs` unit tests cover the plain-Rust core (round-trip,
//! gzip, determinism, the hostile-decode battery, laziness at the decode layer).
//! These run REAL `.as` programs through the built binary to prove the full
//! Value/generator plumbing: the writer handle, the lazy `tarEntries` generator
//! over `for await`, the Tier-1 corrupt-entry protocol, and the used-after-finish
//! Tier-2 panic.

#![cfg(all(feature = "archive", feature = "data"))]

use std::process::Command;

/// Write `src` to a temp file and `ascript run` it, returning (success, stdout, stderr).
fn run(name: &str, src: &str) -> (bool, String, String) {
    let file = std::env::temp_dir().join(format!("ascript_arch_{name}_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Writer → add → finish → tarEntries round-trips names, sizes, modes, isDir, data.
#[test]
fn writer_then_entries_roundtrips() {
    let src = r#"
import { tarWriter, tarEntries } from "std/archive"
import { utf8Decode } from "std/encoding"

let w = tarWriter()
w.add("a.txt", "hello")
w.add("dir/", nil, {dir: true})
w.add("b.bin", "world", {mode: 0o600})
let bytes = w.finish()

for await (e of tarEntries(bytes)) {
  print(`${e.name}|${e.size}|${e.isDir}|${utf8Decode(e.data)!}`)
}
"#;
    let (ok, stdout, stderr) = run("roundtrip", src);
    assert!(ok, "program failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "three entries expected, got: {stdout:?}");
    assert!(lines[0].starts_with("a.txt|5|false|"), "entry 0: {}", lines[0]);
    assert!(lines[0].ends_with("|hello"), "entry 0 data: {}", lines[0]);
    assert!(lines[1].contains("|0|true|"), "dir entry: {}", lines[1]);
    assert!(lines[2].starts_with("b.bin|5|false|"), "entry 2: {}", lines[2]);
    assert!(lines[2].ends_with("|world"), "entry 2 data: {}", lines[2]);
}

/// `tarWriter({gzip:true})` output is gzip-wrapped and `tarEntries` magic-sniffs it.
#[test]
fn gzip_writer_roundtrips_through_entries() {
    let src = r#"
import { tarWriter, tarEntries } from "std/archive"
import { get } from "std/bytes"
import { utf8Decode } from "std/encoding"
let w = tarWriter({gzip: true})
w.add("g.txt", "gzipped-content")
let bytes = w.finish()
// gzip magic 0x1f 0x8b.
print(get(bytes, 0))
print(get(bytes, 1))
for await (e of tarEntries(bytes)) {
  print(`${e.name}=${utf8Decode(e.data)!}`)
}
"#;
    let (ok, stdout, stderr) = run("gzip", src);
    assert!(ok, "program failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "31", "gzip magic byte 0 (0x1f)");
    assert_eq!(lines[1], "139", "gzip magic byte 1 (0x8b)");
    assert_eq!(lines[2], "g.txt=gzipped-content");
}

/// `{deterministic:true}`: two writers with the same adds → byte-identical output.
#[test]
fn deterministic_output_is_byte_identical() {
    let src = r#"
import { tarWriter } from "std/archive"
import { get } from "std/bytes"
fn build() {
  let w = tarWriter({deterministic: true})
  w.add("x.txt", "same", {mtime: 12345})
  w.add("y/", nil, {dir: true, mtime: 99999})
  return w.finish()
}
let a = build()
let b = build()
let same = len(a) == len(b)
let i = 0
while (i < len(a)) {
  if (get(a, i) != get(b, i)) { same = false }
  i = i + 1
}
print(same)
"#;
    let (ok, stdout, stderr) = run("deterministic", src);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout.trim(), "true", "deterministic output must be identical");
}

/// `tarAppend` preserves the originals and appends new entries.
#[test]
fn append_preserves_and_adds() {
    let src = r#"
import { tarWriter, tarEntries, tarAppend } from "std/archive"
import { utf8Decode } from "std/encoding"
let w = tarWriter()
w.add("orig.txt", "original")
let base = w.finish()

let [appended, err] = tarAppend(base, [{name: "added.txt", data: "new"}])
if (err != nil) { print(`ERR:${err.message}`) } else {
  for await (e of tarEntries(appended)) {
    print(`${e.name}=${utf8Decode(e.data)!}`)
  }
}
"#;
    let (ok, stdout, stderr) = run("append", src);
    assert!(ok, "program failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "two entries: {stdout:?}");
    assert_eq!(lines[0], "orig.txt=original");
    assert_eq!(lines[1], "added.txt=new");
}

/// LAZINESS / Tier-1 protocol: a corrupt 2nd entry → entry 1 yields fine, the
/// next pull surfaces a `[nil, err]` pair (NOT a Rust panic, NOT an abort).
#[test]
fn corrupt_entry_yields_first_then_tier1_error() {
    let src = r#"
import { tarWriter, tarEntries } from "std/archive"
import { set } from "std/bytes"

let w = tarWriter()
w.add("good.txt", "fine")
w.add("bad.txt", "corruptme")
let bytes = w.finish()

// Corrupt block 2 (the second header) checksum field at 148..156.
let i = 1024 + 148
while (i < 1024 + 156) {
  set(bytes, i, 255)
  i = i + 1
}

let yielded = 0
let sawError = false
for await (e of tarEntries(bytes)) {
  yielded = yielded + 1
  // An entry object has a `name`; the Tier-1 error sentinel is a [nil, err] pair
  // (an array). `type()` tells them apart.
  if (type(e) == "array") {
    sawError = true
    print(`ERROR_PAIR:${e[1].message}`)
  } else {
    print(`OK:${e.name}`)
  }
}
print(`yielded=${yielded}`)
print(`sawError=${sawError}`)
"#;
    let (ok, stdout, stderr) = run("corrupt", src);
    assert!(ok, "program should not panic/abort: {stderr}\nstdout:{stdout}");
    // Entry 1 yields fine FIRST.
    assert!(stdout.contains("OK:good.txt"), "first entry must yield: {stdout}");
    // Then the corrupt entry surfaces as a Tier-1 error pair.
    assert!(
        stdout.contains("ERROR_PAIR:"),
        "corrupt entry must yield a [nil,err] pair: {stdout}"
    );
    assert!(stdout.contains("sawError=true"), "{stdout}");
}

/// Hostile input through the script surface: non-tar garbage never panics — the
/// generator terminates cleanly (possibly with a trailing error pair), and the
/// program exits 0.
#[test]
fn hostile_bytes_through_script_never_panic() {
    let src = r#"
import { tarEntries } from "std/archive"
import { fromArray } from "std/bytes"
// Random non-tar garbage.
let junk = fromArray([171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171])
let count = 0
for await (e of tarEntries(junk)) {
  count = count + 1
}
print(`done:${count}`)
"#;
    let (ok, stdout, stderr) = run("hostile", src);
    assert!(ok, "hostile bytes must not panic the runtime: {stderr}");
    assert!(stdout.contains("done:"), "{stdout}");
}

/// Using a writer after `finish()` is a Tier-2 panic (the handle is consumed).
#[test]
fn writer_used_after_finish_is_tier2() {
    let src = r#"
import { tarWriter } from "std/archive"
let w = tarWriter()
w.add("a.txt", "x")
let _ = w.finish()
w.add("b.txt", "y")   // already finished → Tier-2
print("unreachable")
"#;
    let (ok, stdout, stderr) = run("after_finish", src);
    assert!(!ok, "use-after-finish must fail (Tier-2): stdout={stdout}");
    assert!(
        stderr.contains("already been finished") || stderr.contains("finish"),
        "expected a finished-handle panic, got: {stderr}"
    );
    assert!(
        !stdout.contains("unreachable"),
        "program ran past the panic: {stdout}"
    );
}
