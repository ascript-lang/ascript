:::eyebrow Standard library

# System & files

These modules give AScript programs access to the host system: the filesystem, the process environment, subprocesses, host OS facts, live system metrics, cryptography, and compression. Each module is imported by its path, for example `import { read, write } from "std/fs"`.

Most of these modules are gated behind Cargo features, all of which are **on by default**:

| Modules | Cargo feature |
| --- | --- |
| `std/fs`, `std/env`, `std/process`, `std/io` | `sys` |
| `std/os` (host facts: pid, platform, arch, cpuCount, hostname, tempDir, inContainer) | `sys` |
| `std/os` (live metrics: memory, swap, cpuUsage, loadAvg, disks, uptime, networkInterfaces, localIp) | `sysinfo` |
| `std/crypto` | `crypto` |
| `std/compress` | `compress` |
| `std/archive` | `archive` |

> [!TIER1]
> Fallible I/O follows the **Tier-1** convention: the function returns a `[value, err]` pair. On success `err` is `nil`; on failure `value` is `nil` and `err` is an error object with a `message` field. Always destructure and check, e.g. `let [text, err] = read(path)`.

> [!TIER2]
> Argument-type misuse (passing a number where a string is expected, an out-of-range length, and so on) is a **Tier-2 panic** that aborts the program. Tier-2 failures are programmer errors, not recoverable conditions — they are never returned through the `[value, err]` pair.

## std/os

Host OS facts and live system metrics.

```ascript
import * as os from "std/os"
```

The **host facts** (`pid`, `platform`, `arch`, `cpuCount`, `hostname`, `tempDir`, `inContainer`) are always available under the `sys` Cargo feature (default-on). The **live metrics** (`memory`, `swap`, `cpuUsage`, `loadAvg`, `disks`, `uptime`, `networkInterfaces`, `localIp`) require the separate `sysinfo` Cargo feature (also default-on). Strip `sysinfo` from a custom build to remove the metric APIs and the `sysinfo` crate dependency.

### Host facts

All host-fact functions are **synchronous** and infallible (they never return a Tier-1 pair).

- `os.pid()` → `number` — the current process ID.
- `os.platform()` → `string` — the OS name: `"macos"`, `"linux"`, `"windows"`, etc.
- `os.arch()` → `string` — the CPU architecture: `"aarch64"`, `"x86_64"`, etc.
- `os.cpuCount()` → `number` — the number of logical CPUs available to the process (falls back to `1` if the OS does not report this).
- `os.hostname()` → `string` — the machine hostname. Returns `"unknown"` if the OS call fails.
- `os.tempDir()` → `string` — the OS temporary directory path.
- `os.inContainer()` → `bool` — heuristic container detection. Returns `true` when the process is running inside a Docker, Podman, or Kubernetes container (Linux: probes `/.dockerenv`, `/run/.containerenv`, and `/proc/1/cgroup`); always `false` on non-Linux. Ungated — succeeds even under `--sandbox`.

```ascript
import * as os from "std/os"

print(os.pid())           // e.g. 12345
print(os.platform())      // "macos"
print(os.arch())          // "aarch64"
print(os.cpuCount())      // e.g. 10
print(os.hostname())      // e.g. "my-machine.local"
print(os.tempDir())       // "/tmp"
print(os.inContainer())   // false (on a dev machine), true (inside Docker)
```

### Live system metrics (sysinfo feature)

> [!NOTE]
> These functions require the `sysinfo` Cargo feature (enabled by default). If you build with `--no-default-features` and omit `sysinfo`, these bindings are not available.

**Memory and swap** — synchronous; snapshot the current allocation from the OS.

- `os.memory()` → `{total, used, free, available}` — RAM in bytes.
- `os.swap()` → `{total, used, free}` — swap space in bytes.

```ascript
let mem = os.memory()
print(`${mem.used} / ${mem.total} bytes used`)
```

**CPU usage** — **async** (must be `await`ed); samples the CPU twice separated by `~200 ms` and returns the average utilization as a percentage (`0`–`100`). The sampling delay is unavoidable — do not call this in a tight loop.

- `await os.cpuUsage()` → `number` — CPU utilization percentage.

```ascript
let pct = await os.cpuUsage()
print(`CPU: ${pct}%`)
```

**Load average** — synchronous.

- `os.loadAvg()` → `{one, five, fifteen}` — 1-, 5-, and 15-minute load averages. On **Windows** the underlying API is unavailable; all three fields return `0.0`.

**Disk information** — synchronous.

- `os.disks()` → `array<{mount, total, free, available}>` — one entry per disk. `free` and `available` report the same value (available space to the process); `sysinfo` 0.31 does not expose a separate "free" vs "available" distinction.

**Uptime** — synchronous.

- `os.uptime()` → `number` — system uptime in seconds.

**Network interfaces** — synchronous.

- `os.networkInterfaces()` → `array<{name, addresses}>` — one entry per network interface. `addresses` is an `array<string>` of IP addresses (both IPv4 and IPv6, without prefixes).

**Best-effort local IP** — synchronous; Tier-1.

- `os.localIp()` → `[string, err]` — the first non-loopback, non-link-local IPv4 address found across all interfaces. Returns `[nil, err]` if no such address is found (e.g. in an air-gapped sandbox or a network-less container).

```ascript
import * as os from "std/os"

let mem  = os.memory()
let load = os.loadAvg()
let up   = os.uptime()
let disks = os.disks()
let ifaces = os.networkInterfaces()
let [localIp, ipErr] = os.localIp()

print(`RAM: ${mem.used}/${mem.total}`)
print(`load: ${load.one} ${load.five} ${load.fifteen}`)
print(`uptime: ${up}s`)
print(`disks: ${len(disks)}`)
print(`interfaces: ${len(ifaces)}`)
print(ipErr == nil ? `local IP: ${localIp}` : "no routable IP found")
```

## std/fs

Filesystem access: read/write/append, metadata, directory listing and recursive walking, pure path helpers, and a recursive `grep`. Path helpers (`join`, `dirname`, `basename`, `extname`, `isAbsolute`) are pure and infallible; everything that touches the disk is Tier-1.

### fs.read

Reads a file as UTF-8 text.

- **path** `string` — the file path.
- **Returns** `[string, err]`. A non-UTF-8 file is reported as an error (`"file is not valid UTF-8"`).

```ascript
import { read } from "std/fs"
let [text, err] = read("config.txt")
if (err != nil) {
  print("could not read: " + err.message)
} else {
  print(text)
}
```

### fs.readBytes

Reads a file as raw bytes.

- **path** `string` — the file path.
- **Returns** `[bytes, err]`.

```ascript
import { readBytes } from "std/fs"
let [data, err] = readBytes("image.png")
print(len(data))
```

### fs.write

Writes data to a file, creating or truncating it.

- **path** `string` — the file path.
- **data** `string | bytes` — a string is written as its UTF-8 bytes.
- **Returns** `[nil, err]`.

```ascript
import { write } from "std/fs"
let [_, err] = write("out.txt", "hello world")
print(err)
```

### fs.append

Appends data to a file, creating it if it does not exist.

- **path** `string` — the file path.
- **data** `string | bytes`.
- **Returns** `[nil, err]`.

```ascript
import { append } from "std/fs"
append("log.txt", "first\n")
append("log.txt", "second\n")
```

### fs.exists

Reports whether a path exists. Infallible.

- **path** `string` — the path to test.
- **Returns** `bool`.

```ascript
import { exists } from "std/fs"
print(exists("config.txt"))
```

### fs.stat

Reads metadata for a path.

- **path** `string` — the path to stat.
- **Returns** `[{size, isFile, isDir, modifiedMs}, err]`, where `size` is the byte length, `isFile`/`isDir` are booleans, and `modifiedMs` is the modification time in Unix milliseconds (or `nil` if unavailable).

```ascript
import { stat } from "std/fs"
let [info, err] = stat("out.txt")
if (err == nil) {
  print(info.size)
  print(info.isFile)
}
```

### fs.mkdir

Creates a directory.

- **path** `string` — the directory to create.
- **recursive** `bool` (optional) — when truthy, creates intermediate directories (like `mkdir -p`). Default `false`.
- **Returns** `[nil, err]`.

```ascript
import { mkdir } from "std/fs"
let [_, err] = mkdir("a/b/c", true)
print(err)
```

### fs.remove

Removes a file or directory.

- **path** `string` — the path to remove.
- **recursive** `bool` (optional) — when truthy and the path is a directory, removes the whole tree. Default `false`.
- **Returns** `[nil, err]`.

```ascript
import { remove } from "std/fs"
remove("out.txt")
remove("a", true)
```

### fs.readDir

Lists the immediate entries of a directory.

- **path** `string` — the directory to list.
- **Returns** `[array, err]` — an array of entry names (not full paths).

```ascript
import { readDir } from "std/fs"
let [names, err] = readDir(".")
for (let name in names) {
  print(name)
}
```

### fs.walk

Recursively walks a directory tree.

- **path** `string` — the root to walk.
- **Returns** `[array, err]` — an array of full paths for every entry found (including the root).

```ascript
import { walk } from "std/fs"
let [paths, err] = walk("src")
print(len(paths))
```

### fs.join

Joins path segments into a single path. Pure and infallible.

- **...parts** `string` — one or more path segments.
- **Returns** `string`.

```ascript
import { join } from "std/fs"
print(join("a", "b", "c"))
```

### fs.dirname

Returns the parent path of a path. Pure and infallible.

- **path** `string`.
- **Returns** `string` — the parent path, or `""` if there is none.

```ascript
import { dirname } from "std/fs"
print(dirname("/x/y/z.txt"))
```

### fs.basename

Returns the final component of a path. Pure and infallible.

- **path** `string`.
- **Returns** `string` — the final component, or `""` if there is none.

```ascript
import { basename } from "std/fs"
print(basename("/x/y/z.txt"))
```

### fs.extname

Returns the extension of a path, including the leading dot. Pure and infallible.

- **path** `string`.
- **Returns** `string` — for example `".txt"`, or `""` if there is no extension.

```ascript
import { extname } from "std/fs"
print(extname("/x/y/z.txt"))
```

### fs.isAbsolute

Reports whether a path is absolute. Pure and infallible.

- **path** `string`.
- **Returns** `bool`.

```ascript
import { isAbsolute } from "std/fs"
print(isAbsolute("/abs/path"))
```

### fs.grep

Searches a directory tree for a regular-expression pattern, line by line.

- **pattern** `string` — a regular expression.
- **dir** `string` — the directory to search.
- **opts** `object` (optional) — see the table below.
- **Returns** `[matches, err]` — an array of match objects. Each match has the shape `{path, line, column, text}`, where `line` and `column` are 1-based and `text` is the full matching line. An invalid regex or glob is reported as a Tier-1 error.

| Option | Type | Default | Meaning |
| --- | --- | --- | --- |
| `glob` | `string` | none | Only files matching this glob are searched (for example `"*.rs"`). |
| `ignoreCase` | `bool` | `false` | Case-insensitive matching. |
| `maxResults` | `number` | none | A value `> 0` caps the result count at exactly that many; absent or `<= 0` means no limit. |
| `respectGitignore` | `bool` | `true` | Honor `.gitignore`, `.ignore`, global excludes, and parent ignores (only inside a git repository). |

> [!WARN]
> Hidden/dotfiles (like `.env` or `.config`) are **always** searched, regardless of `respectGitignore`. Non-UTF-8 / binary files are skipped silently so one bad file does not fail the whole search.

```ascript
import { grep } from "std/fs"
let [matches, err] = grep("TODO", "src", { glob: "*.rs", maxResults: 50 })
for (let m in matches) {
  print(m.path + ":" + m.line + ":" + m.column + " " + m.text)
}
```

## std/env

Access to the process environment: read, set, and unset variables, snapshot all of them, and load a `.env` file.

> [!WARN]
> `set`, `unset`, and `loadDotenv` mutate the **process-global** environment. The change is visible to every subsequent `get`/`vars` call and to every `std/process` spawn in the same process.

### env.get

Reads an environment variable.

- **name** `string` — the variable name.
- **Returns** `string | nil` — the value, or `nil` if the variable is unset.

```ascript
import { get } from "std/env"
print(get("HOME"))
```

### env.set

Sets an environment variable. Mutates the process-global environment.

- **name** `string` — the variable name.
- **value** `string` — the value.
- **Returns** `nil`.

```ascript
import { set, get } from "std/env"
set("MY_VAR", "hello")
print(get("MY_VAR"))
```

### env.unset

Removes an environment variable. Mutates the process-global environment.

- **name** `string` — the variable name.
- **Returns** `nil`.

```ascript
import { unset } from "std/env"
unset("MY_VAR")
```

### env.vars

Snapshots all current environment variables.

- **Returns** `object` — a map of every environment variable to its string value (order arbitrary).

```ascript
import { vars } from "std/env"
let all = vars()
print(all.PATH)
```

### env.loadDotenv

Loads a `.env` file into the process environment.

- **path** `string` (optional) — the file to load. Defaults to `.env`.
- **Returns** `[count, err]` — the number of variables loaded. A missing or unparseable file is a Tier-1 error.

```ascript
import { loadDotenv, get } from "std/env"
let [count, err] = loadDotenv(".env")
print(count)
print(get("DATABASE_URL"))
```

### env.args

Returns the script's trailing CLI arguments — the tokens after `ascript run file.as` that were not consumed by the runner. When `ascript` is not passed extra arguments (or the script is run in the REPL), this returns an empty array.

- Takes no arguments.
- **Returns** `array<string>`.

```ascript
import * as env from "std/env"
let args = env.args()
print(len(args))
if (len(args) > 0) {
  print(args[0])
}
```

> [!NOTE]
> `std/cli`'s `cli.parse(spec)` calls `env.args()` automatically when no `args` argument is supplied, so you rarely need to call `env.args()` directly.

## std/io

Standard-input reading. `std/io` provides three async functions for consuming `stdin`; they share a single internal `BufReader` so that bytes are never silently dropped between calls.

> [!NOTE]
> `std/io` is part of the `sys` Cargo feature (enabled by default).

### io.readLine

Reads one line from stdin, stripping the trailing newline.

- Takes no arguments.
- **Returns** `string | nil` — the line text (without `\n`), or `nil` at EOF.

```ascript
import * as io from "std/io"
let line = await io.readLine()
if (line != nil) {
  print("got: " + line)
}
```

### io.readAll

Reads all remaining stdin as a single UTF-8 string (lossy — invalid bytes become the replacement character).

- Takes no arguments.
- **Returns** `string`.

```ascript
import * as io from "std/io"
let text = await io.readAll()
print(len(text))
```

### io.readLines

Reads every remaining line of stdin and returns them as an array.

- Takes no arguments.
- **Returns** `array<string>` — one element per line, each without the trailing `\n`.

```ascript
import * as io from "std/io"
let lines = await io.readLines()
for (let line of lines) {
  print(line)
}
```

## std/process

Subprocess execution built on the async event loop. There are two entry points, both **async** (they must be `await`ed) and both sharing one options object:

- `process.run` — one-shot: spawn, await completion, and capture output.
- `process.spawn` — streaming: returns a `ChildProcess` handle whose stdio you read and write incrementally.

It also handles **inbound** signals (the signals your own process receives): `process.on` / `process.off` register and remove handlers for `SIGTERM`/`SIGINT`/etc.

> [!TIER1]
> For `process.run`, a **non-zero exit is not an error** — it comes back as a normal result with `success == false`. Only a *spawn failure* (binary not found, permission denied, timeout) is the `err`. Setting `check: true` flips a non-zero exit into a Tier-1 error.

### The shared options object

Both `run` and `spawn` accept an optional third argument. Unknown keys are ignored.

| Option | Type | Default | Meaning |
| --- | --- | --- | --- |
| `cwd` | `string` | inherited | Working directory for the child. |
| `env` | `object` | inherited | Variables to set on the child. A key whose value is `nil` **unsets** that variable. Numbers and booleans are coerced to strings. |
| `clearEnv` | `bool` | `false` | Start from an empty environment instead of inheriting (the `env` map is then applied on top). |
| `stdin` | `string \| bytes` | none | Input written to the child's stdin, then closed (EOF). Used by `run`. |
| `shell` | `bool` | `false` | Run `cmd` through the platform shell (`/bin/sh -c` on unix, `cmd.exe /C` on Windows) instead of executing it directly. Non-portable. |
| `timeout` | `number` | none | Milliseconds before `run` aborts and returns a timeout error (must be non-negative). |
| `check` | `bool` | `false` | For `run`, turn a non-zero exit into a Tier-1 error. |
| `capture` | `string` | `"string"` | How stdout/stderr are captured: `"string"` (lossy UTF-8), `"bytes"` (raw), `"inherit"` (share our stdio; nothing captured), or `"null"` (discard). |

### process.run

Runs a command to completion and captures its output. Async — must be `await`ed.

- **cmd** `string` — the program to run (or the shell command line when `shell: true`).
- **args** `array` (optional) — argument strings. `nil` means no arguments.
- **opts** `object` (optional) — see the options table above.
- **Returns** `[result, err]`. The result object has this shape:

| Field | Type | Meaning |
| --- | --- | --- |
| `stdout` | `string \| bytes` | Captured stdout (kind depends on `capture`). |
| `stderr` | `string \| bytes` | Captured stderr (kind depends on `capture`). |
| `stderrText` | `string` | Captured stderr always decoded as lossy UTF-8 text, for convenient error messages. |
| `code` | `number \| nil` | Exit code, or `nil` if the process was killed by a signal. |
| `signal` | `string \| nil` | Signal name (for example `"SIGTERM"`) on unix if killed by a signal, otherwise `nil`. |
| `success` | `bool` | `true` only when the exit code is `0`. |

```ascript
import { run } from "std/process"
let [result, err] = await run("echo", ["hello"])
if (err != nil) {
  print("spawn failed: " + err.message)
} else {
  print(result.stdout)
  print(result.success)
  print(result.code)
}
```

### process.spawn

Spawns a command and returns a live `ChildProcess` handle for streaming I/O. Async — must be `await`ed.

- **cmd** `string` — the program to run (or the shell command line when `shell: true`).
- **args** `array` (optional) — argument strings. `nil` means no arguments.
- **opts** `object` (optional) — see the options table above (the `stdin`, `timeout`, and `check` options apply to `run`, not `spawn`; stdin is always a pipe here).
- **Returns** `[child, err]` — a `ChildProcess` handle, or an error on spawn failure.

#### ChildProcess handle

The handle exposes a `pid` field plus the following methods. The `stdin`/`stdout`/`stderr` accessors return the corresponding stream handle (a Writer for stdin, Readers for stdout/stderr), or `nil` if that stream was not piped.

- **child.pid** — the process id (a field, not a method); `nil` if unavailable.
- **child.stdin** — the stdin Writer handle.
- **child.stdout** — the stdout Reader handle.
- **child.stderr** — the stderr Reader handle.
- **await child.wait()** — wait for the process to exit. Consumes the child and finalizes its streams. Returns a status object `{code, signal, success}` (same fields as the `run` result). Drain the readers *before* calling `wait()`.
- **child.kill(sig?)** — send a signal. `sig` defaults to `"KILL"`. Accepts `"KILL"`/`"TERM"`/`"INT"`/`"HUP"` (the `SIG` prefix is optional). Returns `nil`.

> [!WARN]
> `kill()` and `"KILL"` are forceful on every platform. `"TERM"`/`"INT"`/`"HUP"` map to the POSIX signal on unix, but Windows has no POSIX signals, so any kill there is a forceful terminate.

#### Reader methods (stdout / stderr)

A Reader degrades gracefully to EOF (returns `nil`) once its stream is exhausted or the child has been `wait()`ed.

- **await reader.read(n?)** — read up to `n` bytes (default 64 KiB). Returns a string or bytes chunk (per `capture`), or `nil` at EOF. `read(0)` returns an empty chunk without advancing. `n` must be a finite, non-negative, in-range count: a negative, `Infinity`/`NaN`, or out-of-range value is a recoverable Tier-2 panic (the size is validated before any buffer is reserved, so it cannot abort the host).
- **await reader.readLine()** — read one line with the trailing `\n` (and optional `\r`) stripped. Returns the line, or `nil` at EOF.
- **await reader.readToEnd()** — read the remaining stream in full. Returns the collected data, or `nil` if already drained.

#### Writer methods (stdin)

- **await writer.write(data)** — write a string or bytes to the child's stdin. Returns `nil`.
- **writer.close()** — close stdin so the child sees EOF. Returns `nil`.

> [!TIER2]
> Writing to a stdin Writer after `close()` (or after `wait()` has finalized it) is a use-after-close Tier-2 panic.

A complete streaming round-trip with `cat`:

```ascript
import { spawn } from "std/process"
let [child, err] = await spawn("cat", [])
if (err != nil) {
  print("spawn failed: " + err.message)
} else {
  await child.stdin.write("line1\n")
  child.stdin.close()
  let line = await child.stdout.readLine()
  print(line)
  let eof = await child.stdout.readLine()
  print(eof)
  let status = await child.wait()
  print(status.success)
}
```

### process.on

Registers a handler for an **inbound** OS signal — the signal your *own* process receives (e.g. a `SIGTERM` from `kill` or an orchestrator shutting the container down). This is the inverse of `child.kill`, which *sends* a signal to a child.

- **signalName** `string` — one of `"SIGTERM"`, `"SIGINT"`, `"SIGHUP"`, `"SIGQUIT"`, `"SIGUSR1"`, `"SIGUSR2"`. An unknown name, or the uncatchable `"SIGKILL"`/`"SIGSTOP"`, is a Tier-2 panic. On Windows only `"SIGINT"` (ctrl-c) is supported; any other name panics.
- **handler** `fn(signalName)` — called with the signal name each time the signal arrives. A second `on` for the same signal **replaces** the handler (last call wins). A panic in the handler is reported and the listener keeps running.
- **Returns** `nil`.

`process.on` is **main-isolate only** — calling it inside a `worker fn` / `worker class` / `worker fn*` is a Tier-2 refusal. Until you register a handler, the signal keeps its OS default (a `SIGTERM` kills the process with exit code 143).

```ascript
import { on } from "std/process"
on("SIGTERM", (sig) => {
  print("shutting down on " + sig)
  exit(0)
})
```

### process.off

Removes a previously-registered handler. The **next** receipt of that signal restores the emulated OS default: the process flushes output and exits with `128 + signo` (e.g. `143` for `SIGTERM`). Removing a signal that was never registered is a no-op. Main-isolate only.

- **signalName** `string` — the signal to stop handling.
- **Returns** `nil`.

```ascript
import { on, off } from "std/process"
on("SIGINT", (sig) => { print("once") ; off("SIGINT") })
```

## std/crypto

Hashing, HMAC, cryptographically secure random bytes, and password hashing. Deterministic hashes return a plain lowercase-hex string. Password hashing is fallible (it draws randomness and encodes a PHC string), so it follows the Tier-1 convention. Hash and HMAC inputs accept a string (encoded as UTF-8) or bytes.

### crypto.sha256

Computes the SHA-256 digest of the input.

- **data** `string | bytes` — the input.
- **Returns** `string` — a 64-character lowercase-hex digest.

```ascript
import { sha256 } from "std/crypto"
print(sha256("abc"))
```

### crypto.sha512

Computes the SHA-512 digest of the input.

- **data** `string | bytes` — the input.
- **Returns** `string` — a 128-character lowercase-hex digest.

```ascript
import { sha512 } from "std/crypto"
print(sha512("abc"))
```

### crypto.md5

Computes the MD5 digest of the input. (MD5 is not collision-resistant; use it only for checksums, not security.)

- **data** `string | bytes` — the input.
- **Returns** `string` — a 32-character lowercase-hex digest.

```ascript
import { md5 } from "std/crypto"
print(md5("abc"))
```

### crypto.hmacSha256

Computes an HMAC-SHA256 tag.

- **key** `string | bytes` — the secret key (any length).
- **data** `string | bytes` — the message.
- **Returns** `string` — a 64-character lowercase-hex tag.

```ascript
import { hmacSha256 } from "std/crypto"
print(hmacSha256("key", "The quick brown fox"))
```

### crypto.hmacSha384

Computes an HMAC-SHA384 tag.

- **key** `string | bytes` — the secret key (any length).
- **data** `string | bytes` — the message.
- **Returns** `string` — a 96-character lowercase-hex tag.

```ascript
import { hmacSha384 } from "std/crypto"
print(hmacSha384("key", "The quick brown fox"))
```

### crypto.hmacSha512

Computes an HMAC-SHA512 tag.

- **key** `string | bytes` — the secret key (any length).
- **data** `string | bytes` — the message.
- **Returns** `string` — a 128-character lowercase-hex tag.

```ascript
import { hmacSha512 } from "std/crypto"
print(hmacSha512("key", "The quick brown fox"))
```

### crypto.timingSafeEqual

Compares two byte sequences in constant time. Use this to compare secrets (HMAC tags, session tokens, API keys) — a naive `a == b` returns as soon as the first differing byte is found, so its running time leaks how many leading bytes matched, letting an attacker recover the secret one byte at a time. `timingSafeEqual` folds an XOR over every byte unconditionally, so its running time is independent of where the inputs differ.

- **a** `string | bytes` — the first value (raw bytes are compared; a string is its UTF-8 bytes).
- **b** `string | bytes` — the second value.
- **Returns** `bool` — `true` iff the byte sequences are equal. A length mismatch returns `false` (length is not secret), never a panic.

```ascript
import { hmacSha256, timingSafeEqual } from "std/crypto"
let expected = hmacSha256(key, body)
if timingSafeEqual(expected, providedSignature) {
  print("signature ok")
}
```

### crypto.randomBytes

Generates cryptographically secure random bytes.

- **n** `number` — the number of bytes; must be a non-negative integer no greater than 16777216 (16 MiB). Out-of-range or non-integer values are a Tier-2 panic.
- **Returns** `bytes`.

```ascript
import { randomBytes } from "std/crypto"
let token = randomBytes(16)
print(len(token))
```

### crypto.hashPassword

Hashes a password with Argon2, returning a self-describing PHC string.

- **password** `string | bytes` — the password.
- **Returns** `[string, err]` — the PHC hash string (begins with `$argon2`).

The Argon2 salt is drawn from a real CSPRNG by default. **Inside a durable `workflow`** (under record/replay), the salt is instead drawn from the workflow's seeded PRNG, so `hashPassword` is **replay-safe**: re-running a recorded workflow produces the byte-identical hash rather than diverging on a fresh random salt. Outside a workflow the salt stays fully random (no security change).

```ascript
import { hashPassword } from "std/crypto"
let [phc, err] = hashPassword("correct horse")
print(err)
```

### crypto.verifyPassword

Verifies a password against an Argon2 PHC string.

- **password** `string | bytes` — the candidate password.
- **phc** `string` — a PHC hash produced by `hashPassword`.
- **Returns** `bool` — `true` on match; a non-match or a malformed PHC string both return `false`.

```ascript
import { hashPassword, verifyPassword } from "std/crypto"
let [phc, _] = hashPassword("secret")
print(verifyPassword("secret", phc))
print(verifyPassword("wrong", phc))
```

### crypto.bcryptHash

Hashes a password with bcrypt.

- **password** `string | bytes` — the password.
- **cost** `number` (optional) — the bcrypt cost factor, an integer in `4..=31`. Defaults to the library default; out-of-range or non-integer costs are a Tier-2 panic.
- **Returns** `[string, err]` — the bcrypt hash string.

Like `hashPassword`, `bcryptHash` is **replay-safe inside a durable `workflow`**: under record/replay the salt is drawn from the workflow's seeded PRNG (reproducible), and outside a workflow it stays a fully random CSPRNG draw (no security change).

```ascript
import { bcryptHash } from "std/crypto"
let [hash, err] = bcryptHash("secret", 10)
print(err)
```

### crypto.bcryptVerify

Verifies a password against a bcrypt hash.

- **password** `string | bytes` — the candidate password.
- **hash** `string` — a bcrypt hash produced by `bcryptHash`.
- **Returns** `bool` — `true` on match; a non-match or a malformed hash both return `false`.

```ascript
import { bcryptHash, bcryptVerify } from "std/crypto"
let [hash, _] = bcryptHash("secret")
print(bcryptVerify("secret", hash))
```

### crypto.crc32

CRC-32 checksum (IEEE polynomial). Fast, non-cryptographic. Accepts a string (encoded as UTF-8) or bytes, and returns the checksum as a number.

- **data** `string | bytes` — the input.
- **Returns** `number` — the CRC-32 value.

> [!TIER2] Panics if the input is not a string or bytes.

```ascript
import * as crypto from "std/crypto"
crypto.crc32("hello")   // 907060870
```

### crypto.xxhash

xxHash-64 (XXH64) with seed 0. Extremely fast, non-cryptographic. Accepts a string (encoded as UTF-8) or bytes, and returns the hash as a 16-character lowercase hex string.

- **data** `string | bytes` — the input.
- **Returns** `string` — 16-character lowercase hex digest.

> [!TIER2] Panics if the input is not a string or bytes.

```ascript
import * as crypto from "std/crypto"
crypto.xxhash("hello")   // "26c7827d889f6da3"
```

## std/compress

Gzip/deflate (de)compression and in-memory zip archives. Compression functions accept a string (encoded as UTF-8) or bytes and return bytes. Decompression takes bytes and is fallible (Tier-1).

> [!TIER2]
> `gunzip`, `inflate`, and `zipExtract` require **bytes** as input — passing a string is an argument-type misuse and a Tier-2 panic. (`gzip`/`deflate` accept strings or bytes.)

### compress.gzip

Compresses data with gzip.

- **data** `string | bytes` — the input.
- **Returns** `bytes`.

```ascript
import { gzip } from "std/compress"
let packed = gzip("hello compress world")
print(len(packed))
```

### compress.gunzip

Decompresses gzip data.

- **data** `bytes` — gzip-compressed bytes.
- **Returns** `[bytes, err]`.

```ascript
import { gzip, gunzip } from "std/compress"
let packed = gzip("hello")
let [raw, err] = gunzip(packed)
print(err)
```

### compress.deflate

Compresses data with raw deflate.

- **data** `string | bytes` — the input.
- **Returns** `bytes`.

```ascript
import { deflate } from "std/compress"
let packed = deflate("the quick brown fox")
print(len(packed))
```

### compress.inflate

Decompresses raw deflate data.

- **data** `bytes` — deflate-compressed bytes.
- **Returns** `[bytes, err]`.

```ascript
import { deflate, inflate } from "std/compress"
let packed = deflate("data")
let [raw, err] = inflate(packed)
print(err)
```

### compress.zipCreate

Builds an in-memory zip archive.

- **entries** `array` — an array of `{name, data}` objects, where `name` is a string and `data` is a string or bytes.
- **Returns** `[bytes, err]` — the zip archive as bytes. A malformed entry (missing/wrong-typed `name` or `data`) is a Tier-2 panic; an archive/I-O failure is a Tier-1 error.

```ascript
import { zipCreate } from "std/compress"
let [archive, err] = zipCreate([
  { name: "a.txt", data: "hello" },
  { name: "b.bin", data: "world" },
])
print(err)
```

### compress.zipExtract

Extracts an in-memory zip archive.

- **data** `bytes` — a zip archive.
- **Returns** `[array, err]` — an array of `{name, data}` objects, where `name` is a string and `data` is bytes.

```ascript
import { zipExtract } from "std/compress"
let [entries, err] = zipExtract(archive)
for (let e in entries) {
  print(e.name + " (" + len(e.data) + " bytes)")
}
```

### compress.zstdCompress / compress.zstdDecompress

zstd (Zstandard) compression. `zstdCompress(src[, level])` accepts a string or
bytes and returns bytes; `level` is optional (1–22, default 3). `zstdDecompress`
takes bytes and is Tier-1.

```ascript
import { zstdCompress, zstdDecompress } from "std/compress"
let packed = zstdCompress("hello", 19)
let [raw, err] = zstdDecompress(packed)
```

### compress.brotliCompress / compress.brotliDecompress

brotli compression. `brotliCompress(src[, quality])` (quality 0–11, default 11);
`brotliDecompress` is Tier-1.

```ascript
import { brotliCompress, brotliDecompress } from "std/compress"
let packed = brotliCompress("hello")
let [raw, err] = brotliDecompress(packed)
```

### compress.tarCreate / compress.tarExtract

tar archives, using the **same `{name, data}` entry shape as zip**. `tarCreate`
takes an array of entries (`data` is bytes or a string) → `[bytes, err]`;
`tarExtract` takes bytes → `[array<{name, data}>, err]`. A malformed entry shape
is a Tier-2 panic; an I/O failure is Tier-1.

```ascript
import { tarCreate, tarExtract } from "std/compress"
let [archive, e1] = tarCreate([{ name: "a.txt", data: "hello" }])
let [entries, e2] = tarExtract(archive)
```

> [!NOTE]
> These are the **one-shot** helpers — they materialize the whole archive at once.
> For **streaming** writers and **lazy** entry generators (so a large archive is
> never fully in memory), **hardened** zip-slip-safe disk extraction, gzip-tar, and
> reproducible (`deterministic`) output, see [`std/archive`](#stdarchive) below.

## std/archive

`std/archive` (Cargo feature `archive`, on by default) is the **streaming** superset
of `std/compress`'s one-shot `tarCreate`/`tarExtract`. Where the compress helpers
build or read a whole archive in one call, `std/archive` lets you assemble a tar
incrementally through a **writer handle** and consume one through a **lazy entries
generator** — so large archives never need to live fully materialized as an entry
array.

The decoder is **hardened against hostile input**: every allocation is bounded
(a per-entry cap rejects a header that declares a giant size before any buffer is
allocated), and a truncated, corrupt, or non-tar stream produces a clean Tier-1
error rather than a panic or an out-of-memory abort.

The module has two planes — **tar** and **zip** — with matching in-memory writers
and lazy entries generators, plus three **filesystem-gated** disk helpers
(`tarExtractTo`, `zipExtractTo`, `tarCreateFromDir`).

> [!NOTE]
> The in-memory functions (`tarWriter`/`zipWriter`, `tarEntries`/`zipEntries`,
> `tarAppend`) are pure and run under `--sandbox`. The disk helpers
> (`tarExtractTo`/`zipExtractTo`/`tarCreateFromDir`) touch the filesystem and are
> **`Fs`-capability gated** — denied under `--sandbox` / `--deny fs` *before* any
> disk access. They additionally require the `sys` feature (they are absent from
> the module when `sys` is off).

### archive.tarWriter

`tarWriter(opts?)` opens a streaming tar writer and returns a handle. The options
object accepts `gzip` (when true, `finish()` returns gzip-wrapped bytes — a
`.tar.gz`) and `deterministic` (when true, each entry's mtime/uid/gid is zeroed so
two identical add-sequences produce **byte-identical** output — reproducible builds).

The handle has two methods. `add(name, data, opts?)` appends one entry — `data` is
bytes, a string, or `nil`, and the per-entry options are `dir` (write a directory
entry — `data` is ignored), `mode` (octal permission bits, default `0o644` for
files / `0o755` for directories), and `mtime`. `finish()` finalizes the archive and
returns the assembled `bytes`; the handle is then consumed, so calling `add` after
`finish` is a Tier-2 panic.

```ascript
import { tarWriter } from "std/archive"

let w = tarWriter({ gzip: true, deterministic: true })
w.add("README.md", "# hello\n")
w.add("src/", nil, { dir: true })
w.add("src/main.as", readFileBytes(), { mode: 0o600 })
let bytes = w.finish()    // gzip-wrapped tar bytes
```

### archive.tarEntries

`tarEntries(bytes)` returns a **lazy generator** over a (optionally gzipped — the
gzip magic is auto-sniffed) tar. Each `next()` decodes one entry and yields an
object `{ name, size, mode, isDir, data }`. Drive it with `for await`:

```ascript
import { tarEntries } from "std/archive"

for await (e of tarEntries(bytes)) {
  if (e.isDir) { continue }
  print(`${e.name} (${e.size} bytes)`)
}
```

If an entry's header is corrupt or the stream is truncated, the **prior entries
still yield normally**, and the generator surfaces the failure as a final
`[nil, err]` Tier-1 pair on the pull that reaches the bad entry — so iteration is
both lazy and fail-safe:

```ascript
for await (e of tarEntries(bytes)) {
  if (type(e) == "array") {          // a [nil, err] error sentinel
    print(`stopped: ${e[1].message}`)
  } else {
    print(e.name)
  }
}
```

### archive.tarAppend

`tarAppend(data, additions)` decodes an existing tar (preserving every entry) and
appends each `{ name, data, mode?, dir? }` of the `additions` array, returning the
new archive bytes. It is Tier-1: a corrupt source archive yields `[nil, err]`.

```ascript
import { tarAppend } from "std/archive"
let [updated, err] = tarAppend(existing, [{ name: "CHANGELOG", data: "v2\n" }])
```

### archive.zipWriter

`zipWriter(opts?)` opens a streaming **zip** writer and returns a handle — the zip
analogue of `tarWriter`. The options object accepts `store` (when true, entries are
**stored without compression** by default; otherwise deflate is used). The handle's
`add(name, data, opts?)` per-entry options are `dir` (write a directory entry),
`mode` (unix permission bits), and `store` (override compression for this entry).
`finish()` returns the assembled zip `bytes`.

```ascript
import { zipWriter } from "std/archive"

let w = zipWriter()
w.add("a.txt", "hello")
w.add("raw.bin", rawBytes, { store: true })   // no compression for this entry
let bytes = w.finish()
```

### archive.zipEntries

`zipEntries(bytes)` returns a **lazy generator** over a zip. Each `next()` yields
`{ name, size, compressedSize, mode, isDir, data }` — `compressedSize` is the
on-disk compressed length (zip carries it; tar does not). A corrupt entry surfaces
as a trailing `[nil, err]` pair, exactly like `tarEntries`.

```ascript
import { zipEntries } from "std/archive"
for await (e of zipEntries(bytes)) {
  print(`${e.name}: ${e.size} → ${e.compressedSize} bytes`)
}
```

### archive.tarExtractTo / archive.zipExtractTo

`tarExtractTo(data, dest, opts?)` and `zipExtractTo(data, dest, opts?)` extract an
archive's contents to the `dest` directory (created if missing). Both return
`[dest, err]` (Tier-1). They are **`Fs`-capability gated** (denied under
`--sandbox` / `--deny fs`).

These functions implement a **bulletproof zip-slip / path-traversal defense** —
extracting a hostile archive is otherwise an arbitrary-file-write vulnerability:

- The destination is **canonicalized first** and becomes the jail root.
- Each entry name is **lexically normalized** component-by-component (splitting on
  both `/` and `\`, processing `.`/`..` *without touching the filesystem* — no
  TOCTOU). An entry that would **escape** the root (a `..` past the top, an
  **absolute** path, a Windows **drive** `C:` or **UNC** `\\server`, or an embedded
  **NUL** byte) is rejected with a Tier-1 error **naming the offending entry**, and
  the lexically-joined write path is re-checked with `starts_with(dest)` as a
  belt-and-suspenders guard.
- **Symlinks and hardlinks are skipped by default** (never materialized) — this
  defeats the **second-order** attack where one entry plants a symlink and a later
  entry writes *through* it. Pass `{ links: "error" }` to fail the extract with a
  Tier-1 error on the first link instead. Parent directories are created
  **without following an existing symlink**, so a pre-existing symlinked subdir in
  `dest` cannot be used to escape.
- The extract **fails on the first hostile entry** and best-effort removes anything
  already written — nothing outside `dest` is ever written, on any input.

```ascript
import { tarExtractTo, zipExtractTo } from "std/archive"

let [out, err] = tarExtractTo(tarGzBytes, "/tmp/unpacked")
if (err != nil) { print(`extract refused: ${err.message}`) }

// Strict mode: any symlink entry is a hard error.
let [_, e2] = zipExtractTo(zipBytes, "/tmp/zip-out", { links: "error" })
```

### archive.tarCreateFromDir

`tarCreateFromDir(dir, opts?)` walks `dir` recursively and builds a tar of its
files and directories, returning `[bytes, err]`. Options: `gzip` (gzip-wrap) and
`deterministic` (zero each entry's mtime/uid/gid **and** sort entries by path, so
two runs over the same tree produce **byte-identical** output). Symlinks inside the
tree are **skipped** (never embedded), mirroring the extraction defense. It is
**`Fs`-capability gated**.

```ascript
import { tarCreateFromDir } from "std/archive"
let [bytes, err] = tarCreateFromDir("./dist", { gzip: true, deterministic: true })
```

### Examples

- [`examples/archive_roundtrip.as`](https://github.com/ascript-lang/ascript/blob/main/examples/archive_roundtrip.as) — tar, tar.gz, and zip round-trips in memory, with deterministic byte-stability.
- [`examples/advanced/backup_tool.as`](https://github.com/ascript-lang/ascript/blob/main/examples/advanced/backup_tool.as) — walk a directory → reproducible deterministic `tar.gz` → verify the entries.
