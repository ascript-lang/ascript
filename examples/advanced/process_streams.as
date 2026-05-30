// process_streams.as
// ---------------------------------------------------------------------------
// Running and streaming to/from child processes with std/process (async).
//   process.run(cmd, args)   -> [result, err]   (buffered: waits, captures all)
//       result = { stdout, stderr, code, signal, success, ... }
//   process.spawn(cmd, args) -> [child, err]     (streaming, long-lived)
//       child.stdin.write(data) / child.stdin.close()
//       child.stdout.readLine() -> string | nil  (nil == EOF)
//       child.wait()            -> { code, signal, success }
//
// All the async work lives inside `main`, which we `await` at the bottom.
// ---------------------------------------------------------------------------

import * as process from "std/process"
import * as array from "std/array"

async fn main() {
  // --- 1. buffered: run `echo hello` and capture its output -------------
  print("=== process.run ===")
  let [result, runErr] = await process.run("echo", ["hello"])
  if (runErr != nil) {
    print(`run failed: ${runErr.message}`)
    return
  }
  // stdout includes echo's trailing newline; show it as-is plus a flag.
  print(`stdout  = ${result.stdout}`)
  print(`success = ${result.success}`)
  print(`code    = ${result.code}`)

  // --- 2. streaming: pipe lines THROUGH `cat` ---------------------------
  // `cat` with no args echoes stdin to stdout, so it's a clean way to show
  // write-then-read streaming over real OS pipes.
  print("\n=== process.spawn (streaming through cat) ===")
  let [child, spawnErr] = await process.spawn("cat", [])
  if (spawnErr != nil) {
    print(`spawn failed: ${spawnErr.message}`)
    return
  }

  // Feed three lines in, then close stdin so `cat` sees EOF and exits.
  await child.stdin.write("line1\n")
  await child.stdin.write("line2\n")
  await child.stdin.write("line3\n")
  await child.stdin.close()

  // Drain stdout line by line until readLine returns nil (EOF).
  let received = []
  let line = await child.stdout.readLine()
  while (line != nil) {
    array.push(received, line)
    line = await child.stdout.readLine()
  }

  print(`read ${len(received)} line(s) back from cat:`)
  for (l of received) {
    print(`  -> ${l}`)
  }

  // Reap the child and report its exit status.
  let status = await child.wait()
  print(`child exit: success=${status.success}, code=${status.code}`)
}

await main()
