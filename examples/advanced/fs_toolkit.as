// fs_toolkit.as
// ---------------------------------------------------------------------------
// A filesystem walkthrough using std/fs against a scratch directory under /tmp.
// It demonstrates:
//   - path helpers: fs.join / fs.basename / fs.dirname / fs.extname
//   - fs.mkdir(path, recursive)
//   - fs.write / fs.read
//   - fs.stat (size, isFile, ...)
//   - fs.readDir (immediate children) and fs.walk (recursive)
//   - fs.grep(pattern, dir, { glob }) -> [{path, line, column, text}]
//   - fs.remove(path, recursive) for cleanup
//
// Fallible fs calls return [value, err]; we check every one. Pure path helpers
// (join/basename/dirname/extname) return a string directly.
// ---------------------------------------------------------------------------

import * as fs from "std/fs"

const ROOT = "/tmp/ascript_fs_demo"

fn main() {
  // --- path helpers (no I/O, no error pair) -----------------------------
  let logPath = fs.join(ROOT, "logs", "app.txt")
  print("=== Path helpers ===")
  print(`join     -> ${logPath}`)
  print(`basename -> ${fs.basename(logPath)}`)
  print(`dirname  -> ${fs.dirname(logPath)}`)
  print(`extname  -> ${fs.extname(logPath)}`)

  // --- start from a clean slate -----------------------------------------
  // Remove any leftovers from a previous run (ignore "not found").
  fs.remove(ROOT, true)

  // --- create the directory tree (recursive) ----------------------------
  let [_m1, mkErr] = fs.mkdir(fs.join(ROOT, "logs"), true)
  if (mkErr != nil) {
    print(`mkdir failed: ${mkErr.message}`)
    return
  }
  print(`\nCreated tree under ${ROOT}`)

  // --- write a few files ------------------------------------------------
  let files = [
    { path: fs.join(ROOT, "readme.txt"), body: "AScript fs toolkit demo\nplain text file\n" },
    { path: fs.join(ROOT, "logs", "app.txt"), body: "INFO  boot ok\nWARN  disk low\nERROR  out of memory\n" },
    { path: fs.join(ROOT, "logs", "access.txt"), body: "GET / 200\nGET /x 404\nERROR upstream\n" },
  ]
  for (f of files) {
    let [_w, wErr] = fs.write(f.path, f.body)
    if (wErr != nil) {
      print(`write ${f.path} failed: ${wErr.message}`)
      return
    }
  }
  print(`Wrote ${len(files)} files`)

  // --- read one back ----------------------------------------------------
  let [readme, rErr] = fs.read(fs.join(ROOT, "readme.txt"))
  if (rErr != nil) {
    print(`read failed: ${rErr.message}`)
    return
  }
  print(`\n=== readme.txt ===\n${readme}`)

  // --- stat: inspect file metadata --------------------------------------
  let [st, sErr] = fs.stat(fs.join(ROOT, "logs", "app.txt"))
  if (sErr != nil) {
    print(`stat failed: ${sErr.message}`)
    return
  }
  print(`stat app.txt -> size=${st.size} bytes, isFile=${st.isFile}, isDir=${st.isDir}`)

  // --- readDir: immediate children of ROOT ------------------------------
  let [entries, dErr] = fs.readDir(ROOT)
  if (dErr != nil) {
    print(`readDir failed: ${dErr.message}`)
    return
  }
  print(`\n=== readDir ${ROOT} (${len(entries)} entries) ===`)
  for (e of entries) {
    print(`  - ${e}`)
  }

  // --- walk: every path in the tree, recursively ------------------------
  let [paths, wkErr] = fs.walk(ROOT)
  if (wkErr != nil) {
    print(`walk failed: ${wkErr.message}`)
    return
  }
  print(`\n=== walk ${ROOT} (${len(paths)} paths) ===`)
  for (p of paths) {
    print(`  ${p}`)
  }

  // --- grep: find "ERROR" across *.txt files ----------------------------
  let [matches, gErr] = fs.grep("ERROR", ROOT, { glob: "*.txt" })
  if (gErr != nil) {
    print(`grep failed: ${gErr.message}`)
    return
  }
  print(`\n=== grep "ERROR" in *.txt (${len(matches)} matches) ===`)
  for (m of matches) {
    // Each match is { path, line, column, text }.
    print(`  ${fs.basename(m.path)}:${m.line}:${m.column}  ${m.text}`)
  }

  // --- cleanup ----------------------------------------------------------
  let [_rm, rmErr] = fs.remove(ROOT, true)
  if (rmErr != nil) {
    print(`remove failed: ${rmErr.message}`)
    return
  }
  print(`\nRemoved ${ROOT} (recursive). Done.`)
}

main()
