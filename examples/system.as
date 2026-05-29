// System modules capstone (Milestone 13).
//
// A single program exercising every M13 system module end-to-end with
// DETERMINISTIC operations (no uuid/time/random values are asserted), so its
// output is stable and can be pinned in an integration test.
//
//   std/fs       — write + read + grep
//   std/crypto   — sha256
//   std/compress — gzip -> gunzip round-trip
//   std/sqlite   — in-memory create / insert / query
//   std/process  — run a subprocess (echo) and read its stdout
//   std/env      — set + get an environment variable
//   std/encoding — utf8Decode the gunzipped bytes
//
// Throwaway error slots use DISTINCT names (`_e1`, `_e2`, ...): `_` is a real
// identifier in AScript, so reusing it in one scope is a redefinition error.

import * as fs from "std/fs"
import * as crypto from "std/crypto"
import * as compress from "std/compress"
import * as sqlite from "std/sqlite"
import * as process from "std/process"
import * as env from "std/env"
import * as encoding from "std/encoding"

// fs: write a temp file, read it back, then grep it.
let path = "/tmp/ascript_m13_demo.txt"
let [_w, _e1] = fs.write(path, "alpha\nbeta TODO\ngamma TODO\n")
let [content, _e2] = fs.read(path)
print(len(content))
let [matches, _e3] = fs.grep("TODO", "/tmp", { glob: "ascript_m13_demo.txt" })
print(len(matches))
print(matches[0].line)

// crypto: deterministic SHA-256 of a known string.
print(crypto.sha256("abc"))

// compress: gzip then gunzip a string and confirm the round-trip is lossless.
let original = "the quick brown fox the quick brown fox"
let gz = compress.gzip(original)
let [back, _e4] = compress.gunzip(gz)
let [text, _e5] = encoding.utf8Decode(back)
print(text == original)

// sqlite: in-memory database, create + insert + parameterized query.
let [db, _e6] = sqlite.open(":memory:")
let [_c1, _e7] = db.exec("CREATE TABLE t(id INTEGER, name TEXT)")
let [_c2, _e8] = db.exec("INSERT INTO t VALUES (?, ?)", [1, "ada"])
let [rows, _e9] = db.query("SELECT name FROM t WHERE id = ?", [1])
print(rows[0].name)
db.close()

// process: run echo and read its captured stdout.
let [result, _e10] = await process.run("echo", ["hello-from-subprocess"])
print(result.stdout)
print(result.success)

// env: set then read an environment variable.
env.set("ASC_M13_DEMO", "demo-value")
print(env.get("ASC_M13_DEMO"))

// cleanup
fs.remove(path)
