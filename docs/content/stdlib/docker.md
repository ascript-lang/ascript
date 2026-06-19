::: eyebrow Standard library

# Docker (`std/docker`)

`std/docker` is a typed client for the local Docker Engine API over its Unix-domain
socket. It connects to the daemon, negotiates a supported API version, and exposes a
unary API for containers and images, streaming APIs for logs/events/pull progress, and
an exec convenience for running commands in containers.

## Capabilities

> [!NOTE]
> `std/docker` requires **both** `net` and `process` — the Docker socket is
> host-root-equivalent: anyone who can POST to `/containers/create` can bind-mount
> `/` and spawn arbitrary host processes. Denying either cap is sufficient to block
> all `docker.*` calls:
>
> - `--deny net` → `capability 'net' denied`
> - `--deny process` → `capability 'process' denied`
> - `--sandbox` → `capability 'net' denied` (net is checked first)
>
> The dual-cap requirement is checked at the `call_stdlib` dispatch gate — before any
> socket I/O — so probing for a daemon under a reduced cap set produces an immediate
> denial, never a partially-executed request. See [Capabilities & sandboxing](caps).

`std/docker` is Unix-only. On Windows every entry point raises a Tier-2 panic:
`Docker is only supported on Unix`.

## Connecting

```js
import * as docker from "std/docker"

// Default socket (/var/run/docker.sock):
let [d, err] = await docker.connect()
if (err != nil) { print("connect failed: " + err.message); exit(1) }
print(d.apiVersion)   // negotiated version, e.g. "1.43"
print(d.socketPath)   // resolved path, e.g. "/var/run/docker.sock"

// Explicit socket (rootless docker, Podman socket, etc.):
let [d2, e2] = await docker.connect({ socketPath: "/run/user/1000/docker.sock" })
```

`docker.connect(opts?)` resolves the daemon socket in this order:

1. `opts.socketPath` — an explicit Unix socket path.
2. `$DOCKER_HOST` — a `unix://<path>` is honored; a `tcp://…` host is a Tier-1 error
   (Docker over TCP is **not** supported — use a Unix socket).
3. The default `/var/run/docker.sock`.

On connect the client probes `GET /v1.24/version`, reads the daemon's `ApiVersion`,
and clamps it to the supported range `[1.24, 1.43]`. A daemon below the `1.24` floor
is a Tier-1 error. A failed/unreachable socket is also a Tier-1 `[nil, err]` — probing
for a daemon is legitimate.

The returned handle exposes two readable fields: `d.apiVersion` (the negotiated
version string) and `d.socketPath` (the resolved socket path).

## Error tiers

`std/docker` follows the stdlib-wide tier convention:

- **Tier-1 (recoverable) `[nil, err]` pairs:** daemon I/O errors (socket unreachable,
  connection refused, transport error), non-2xx daemon responses (`err.message` + `err.statusCode`),
  version below the floor, TCP `$DOCKER_HOST` (not supported). These are data — check and handle them.
- **Tier-2 panics (programmer errors):** a non-string container/image id, a wrong argument type, calling
  `process.on` from a worker isolate, `attachStdin: true` in exec.

## The unary API

Every method returns a `[value, err]` pair. A non-2xx daemon response is an error
pair `[nil, { message, statusCode }]` — `message` is the daemon's `{"message":…}`
text and `statusCode` is the HTTP status. A 204 No Content response is `[nil, nil]`.

```js
import * as docker from "std/docker"

let [d, err] = await docker.connect()
if (err != nil) { print("docker: unavailable: " + err.message); exit(1) }

let [cs, e1] = await d.containers({ all: true })   // list containers
let [c,  e2] = await d.inspect("abc123")           // inspect one container
let [n,  e3] = await d.create({ Image: "nginx:latest" })
let [_,  e4] = await d.start("abc123")             // 204 → [nil, nil]
let [_,  e5] = await d.stop("abc123")
let [_,  e6] = await d.remove("abc123", { force: true })
let [is, e7] = await d.images({})                  // list images
d.close()
```

| Method | Returns | Description |
| --- | --- | --- |
| `ping()` | `[true, err]` | Daemon liveness check (`/_ping`). Success always returns `true`. |
| `version()` | `[object, err]` | Daemon version info (`{ Version, ApiVersion, Os, Arch, … }`). |
| `info()` | `[object, err]` | Daemon system info (`{ Containers, ContainersRunning, Images, … }`). |
| `containers(opts?)` | `[array<object>, err]` | List containers (`{ all?, filters? }`). |
| `inspect(id)` | `[object, err]` | Inspect a container by id or name. |
| `create(config)` | `[id, err]` | Create a container from a config object; returns the new container id. |
| `start(id)` | `[nil, err]` | Start a container (204 → `[nil, nil]`). |
| `stop(id)` | `[nil, err]` | Stop a container (204 → `[nil, nil]`). |
| `restart(id)` | `[nil, err]` | Restart a container. |
| `wait(id)` | `[int, err]` | Block until a container exits; returns the exit status code. |
| `remove(id, opts?)` | `[nil, err]` | Remove a container (`{ force? }`). |
| `images(opts?)` | `[array<object>, err]` | List images (`{ filters? }`). |
| `removeImage(ref, opts?)` | `[array<object>, err]` | Remove an image (`{ force? }`). Returns the list of deleted layers. |
| `close()` | `nil` | Release the client handle. Streams opened from it outlive it. |

`filters` is JSON-encoded onto the query string. Key names pass through as the Engine
API returns them (`Id`, `Names`, `State`, …) — no renaming layer. A non-string id is
a Tier-2 panic.

## Example: connect, inspect, list

The `examples/docker_info.as` example shows a fully-handled connect → version →
info → container-list flow with deterministic exit on every error path:

```js
import * as docker from "std/docker"
import * as env from "std/env"

let sock = env.get("DOCKER_SOCK") ?? "/var/run/docker.sock"
let [client, connErr] = await docker.connect({ socketPath: sock })
if (connErr != nil) {
  print("docker: unavailable")
  exit(0)
}

print(`apiVersion: ${client.apiVersion}`)

let [ver, verErr] = await client.version()
if (verErr == nil) {
  print(`engine: ${ver.Version}`)
  print(`os: ${ver.Os}/${ver.Arch}`)
}

let [containers, listErr] = await client.containers({ all: true })
if (listErr == nil) {
  for (c in containers) {
    print(`  ${c.Names[0]} (${c.Image}) ${c.State}`)
  }
}

client.close()
```

## Streaming: `logs`, `events`, `pull`

The three streaming verbs return a `dockerStream` handle that is iterable with
`for await`. Each `stream.next()` call returns `[item, err]`; `[nil, nil]` signals
the end of the stream. Call `stream.close()` to abort early and release the connection.

### Log item shape (`{stream, text}`)

Each log item is an object with two fields:

| Field | Type | Value |
| --- | --- | --- |
| `stream` | `string` | `"stdout"` or `"stderr"` |
| `text` | `string` | The log text (UTF-8-lossy) |

```js
let [logs, logsErr] = await d.logs("web", {
  follow: true, stdout: true, stderr: true, tail: "100"
})
if (logsErr != nil) { print("logs error: " + logsErr.message); exit(1) }

for await (entry in logs) {
  print(`[${entry.stream}] ${entry.text}`)
}
logs.close()
```

**TTY auto-detection:** when the container was started without a TTY, the daemon
multiplexes stdout and stderr using an 8-byte frame header. When it has a TTY, the
daemon sends a raw byte stream. `std/docker` auto-detects the framing on the first
8 bytes — you do not need to pass a `tty` flag to `logs`.

`d.logs(id, opts?)` options:

| Option | Type | Default | Meaning |
| --- | --- | --- | --- |
| `follow` | bool | `false` | Keep the connection open and stream new lines. |
| `stdout` | bool | `true` | Include stdout. |
| `stderr` | bool | `true` | Include stderr. |
| `tail` | string | (all) | Number of lines from the end, e.g. `"100"`. |
| `since` | string | (all) | Unix timestamp or RFC3339 string. |
| `until` | string | (now) | Unix timestamp or RFC3339 string. |
| `timestamps` | bool | `false` | Prefix each line with a timestamp. |

### Events stream

`d.events(opts?)` opens `GET /events` as a stream of newline-delimited JSON objects.
Each item is the decoded event object (`{ Action, Actor, Type, … }`). The stream never
ends unless `until` is set or you close it.

```js
let [events, evtErr] = await d.events({
  filters: { event: ["die"], label: ["app=web"] }
})
if (evtErr != nil) { print("events error: " + evtErr.message); exit(1) }

for await (ev in events) {
  if (ev.Action == "die") {
    print(`container died: ${ev.Actor.ID}`)
  }
}
events.close()
```

### Pull progress stream

`d.pull(ref)` opens `POST /images/create` as a stream of JSON-lines progress objects
(`{ status, progressDetail, id }`). An in-stream `{"error": …}` line from the registry
surfaces as a terminal `[nil, err]` item (the stream is then ended).

```js
let [progress, pullErr] = await d.pull("nginx:latest")
if (pullErr != nil) { print("pull error: " + pullErr.message); exit(1) }

for await (p in progress) {
  if (p.status != nil) { print(`pull: ${p.status}`) }
}
progress.close()
```

## Running a command (`exec`)

`d.exec(containerId, opts)` is the convenience composition: it creates an exec, starts
it (hijacking the connection via HTTP/1.1 `101 Upgrade`), drains the demuxed output,
and inspects for the exit code — returning `{ exitCode, code, stdout, stderr }` (the
`code` field is an alias for `exitCode`, mirroring `process.run`'s shape).

```js
let [res, err] = await d.exec("web", { cmd: ["echo", "hi"] })
if (err != nil) { print(err.message); exit(1) }
print(res.exitCode)  // 0
print(res.stdout)    // "hi\n"
print(res.stderr)    // ""
```

The three steps are also available individually:

| Method | Description |
| --- | --- |
| `execCreate(id, opts)` | Create an exec on a container (`{ cmd, env?, workingDir?, user?, attachStdout?, attachStderr?, tty? }`) → `[execId, err]`. |
| `execStart(execId, opts?)` | Start an exec via a `101` connection hijack → `[stream, err]`; the stream demuxes stdout/stderr (raw stdout under `{ tty: true }`). |
| `execInspect(execId)` | Inspect an exec's status → `[{ ExitCode, Running, … }, err]`. |

`opts.cmd` is the argv array. Interactive stdin attach is out of scope in v1:
`attachStdin: true` is a Tier-2 panic — run without stdin, or use `std/process`.

## The supervisor pattern

`examples/advanced/docker_supervisor.as` shows a production-shaped supervisor that
watches the event stream for `die` events, restarts supervised containers under a
bounded retry budget (`task.retry`), tails their logs after restart, and shuts down
cleanly on `SIGTERM`. See [Deploying & containers](../deploying) for the full
container deployment story.

## See also

- [Deploying & containers](../deploying) — Dockerfile pattern, SIGTERM drain, cgroup sizing.
- [Capabilities & sandboxing](caps) — dual-cap requirement detail.
- [Networking & HTTP → std/net/unix](net#stdnetunix) — the underlying UDS transport.
