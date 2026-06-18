# Docker (`std/docker`)

`std/docker` is a typed client for the local Docker Engine API over its Unix-domain
socket. It connects to the daemon, negotiates a supported API version, and exposes a
small unary API for containers and images.

> Capabilities: `std/docker` requires **both** `net` and `process` — running the
> Engine API talks to the network *and* can spawn host processes. A run under
> `--deny net`, `--deny process`, or `--sandbox` denies every `docker.*` call.

## Connecting

```js
import * as docker from "std/docker"

let [d, err] = await docker.connect({ socketPath: "/var/run/docker.sock" })
if (err != nil) { print("connect failed: " + err.message); exit(1) }
print(d.apiVersion) // the negotiated API version, e.g. "1.43"
```

`docker.connect(opts?)` resolves the daemon socket in this order:

1. `opts.socketPath` — an explicit Unix socket path.
2. `$DOCKER_HOST` — a `unix://<path>` is honored; a `tcp://…` host is a Tier-1 error
   (Docker over TCP is **not** supported — use a Unix socket).
3. The default `/var/run/docker.sock`.

On connect the client probes `GET /v1.24/version`, reads the daemon's `ApiVersion`,
and clamps it to the supported range `[1.24, 1.43]`. A daemon below the `1.24` floor
is a Tier-1 error.

The returned handle exposes two readable fields: `d.apiVersion` (the negotiated
version string) and `d.socketPath` (the resolved socket path).

## The unary API

Every method returns a `[value, err]` pair. A non-2xx daemon response is an error
pair `[nil, { message, statusCode }]` — `message` is the daemon's `{"message":…}`
text and `statusCode` is the HTTP status. A 204 No Content response is `[nil, nil]`.

```js
let [cs, e1] = await d.containers({ all: true })   // list containers
let [c,  e2] = await d.inspect("abc123")           // inspect one container
let [n,  e3] = await d.create({ Image: "nginx:latest" })
let [_,  e4] = await d.start("abc123")             // 204 → [nil, nil]
let [_,  e5] = await d.stop("abc123")
let [_,  e6] = await d.remove("abc123", { force: true })
let [is, e7] = await d.images({})                  // list images
d.close()
```

| Method | Description |
| --- | --- |
| `ping()` | Daemon liveness check (`/_ping`). |
| `version()` / `info()` | Daemon version / system info. |
| `containers(opts?)` | List containers (`{ all, filters }`). |
| `inspect(id)` | Inspect a container. |
| `create(config)` | Create a container from a config object. |
| `start(id)` / `stop(id)` / `restart(id)` | Lifecycle control. |
| `wait(id)` | Block until a container exits. |
| `remove(id, opts?)` | Remove a container (`{ force }`). |
| `images(opts?)` | List images (`{ filters }`). |
| `removeImage(id, opts?)` | Remove an image (`{ force }`). |
| `close()` | Release the client handle. |

`filters` is JSON-encoded onto the query string. A non-string id is a Tier-2 panic.

## Running a command (`exec`)

`d.exec(containerId, opts)` is the convenience composition: it creates an exec, starts
it (hijacking the connection), drains the output, and inspects for the exit code —
returning `{ exitCode, code, stdout, stderr }` (the `code` field mirrors
`process.run`'s shape).

```js
let [res, err] = await d.exec("web", { cmd: ["echo", "hi"] })
if (err != nil) { print(err.message); exit(1) }
print(res.exitCode)  // 0
print(res.stdout)    // "hi\n"
```

The three steps are also available individually:

| Method | Description |
| --- | --- |
| `execCreate(id, opts)` | Create an exec on a container (`{ cmd, env?, workingDir?, user?, attachStdout?, attachStderr?, tty? }`) → `[execId, err]`. |
| `execStart(execId, opts?)` | Start an exec via a `101` connection hijack → `[stream, err]`; the stream demuxes stdout/stderr like `logs` (raw stdout under `{ tty: true }`). |
| `execInspect(execId)` | Inspect an exec's status → `[{ ExitCode, Running, … }, err]`. |

`opts.cmd` is the argv array. Interactive stdin attach is out of scope in v1:
`attachStdin: true` is a Tier-2 panic — run without stdin, or use `std/process`.
