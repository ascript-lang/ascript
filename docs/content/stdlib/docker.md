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
