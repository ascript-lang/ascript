::: eyebrow Introduction

# Deploying & containers

AScript is designed to be a first-class container citizen: it drains gracefully on
`SIGTERM`, sizes its worker pool to the cgroup quota instead of the host's full core
count, ships as a single self-contained native binary, and can manage the Docker
daemon directly from script. This page walks through the production deployment story
end to end.

## Quickstart — scaffold a server project

`ascript init` scaffolds a production-ready HTTP service with all the pieces wired up
out of the box:

```text
ascript init --template server ./my-service
cd my-service
ascript run main.as            # run locally
docker build -t my-service .  # build a container image
```

The generated files are:

| File | What it does |
| ---- | ------------ |
| `main.as` | HTTP server with routes, a `/healthz` probe, `SIGTERM` drain, and `workers: 0` for cgroup-aware sizing. |
| `Dockerfile` | Multi-stage build: compile stage produces a self-contained native binary via `ascript build --native`; runtime stage uses `debian:bookworm-slim` with a non-root user and `STOPSIGNAL SIGTERM`. |
| `.dockerignore` | Keeps sources out of the runtime layer. |
| `ascript.toml` | Project manifest with a commented `[capabilities]` example. |

See [The ascript CLI → `ascript init`](cli#ascript-init) for full flag documentation.

## The Dockerfile pattern

A multi-stage build keeps the runtime image small. The compile stage uses the full
AScript toolchain to produce a self-contained binary (all bytecode + the embedded VM);
the runtime stage copies only that binary into a minimal base:

```dockerfile
# Stage 1: compile a self-contained native binary
FROM debian:bookworm-slim AS build
RUN apt-get update && apt-get install -y curl ca-certificates && \
    curl -fsSL https://github.com/ascript-lang/ascript/releases/latest/download/install.sh | sh
COPY . /app
WORKDIR /app
RUN ascript build --native main.as -o /app/server

# Stage 2: runtime-only image
FROM debian:bookworm-slim
RUN adduser --disabled-password --no-create-home app
COPY --from=build /app/server /usr/local/bin/server
STOPSIGNAL SIGTERM
USER app
EXPOSE 8080
CMD ["/usr/local/bin/server"]
```

> [!NOTE]
> **RT upgrade point (coming):** once `ascript build --oci` ships (the RT spec), you
> can replace the multi-stage approach with a single command that produces a
> deterministic, Docker-free OCI image tarball from a musl-linked runtime stub — no
> Dockerfile, no Docker daemon needed to build. The multi-stage pattern above works
> with any Docker daemon today.

## Graceful shutdown (SIGTERM → drain)

Kubernetes and Docker both send `SIGTERM` before forcefully killing a container. Wire
it up with one inbound-signal handler:

```js
import * as server from "std/http/server"
import * as process from "std/process"
import * as log from "std/log"
import * as env from "std/env"

let srv = server.create()
srv.get("/", async (req) => ({ status: 200, body: "hello" }))
srv.get("/healthz", (req) => ({ status: 200, body: { status: "ok" } }))

// Register a handler for SIGTERM (and SIGINT for local dev).
// process.on is main-isolate only — it must be called before serve.
process.on("SIGTERM", () => {
  log.info("SIGTERM received — draining")
  srv.shutdown()
})
process.on("SIGINT", () => srv.shutdown())

let port = parseInt(env.get("PORT") ?? "8080")
log.info("listening", { port })
await srv.serve({
  port,
  workers: 0,           // cgroup-aware: uses the cgroup CPU quota when set
  drainTimeout: 8000,   // abort in-flight after 8 s (match your k8s terminationGracePeriodSeconds)
  onShutdown: () => log.info("drain started"),
})
```

The sequence on `SIGTERM`:
1. `process.on` handler fires → `srv.shutdown()` signals all accept loops.
2. Accept loops stop accepting new connections; in-flight handlers finish.
3. After `drainTimeout` ms, any remaining handlers are cancelled (connections reset).
4. `serve` resolves → the program exits 0.

> [!NOTE]
> `process.on` / `process.off` register and remove inbound-signal handlers for your
> **own** process (`SIGTERM`, `SIGINT`, `SIGHUP`, `SIGQUIT`, `SIGUSR1`, `SIGUSR2`).
> Handlers are **main-isolate only** — calling them inside a `worker fn` or `worker
> class` is a Tier-2 refusal. See [System & files → process.on](stdlib/system#processonsignalname-handler).

## The `/healthz` probe

Kubernetes readiness and liveness probes need a lightweight endpoint that does not hit
any upstream dependency:

```js
srv.get("/healthz", (req) => ({
  status: 200,
  body: { status: "ok", uptime: process.uptime() },
}))
```

Configure the probe in your deployment manifest with a `failureThreshold` high enough
to survive a slow drain, and set `terminationGracePeriodSeconds` to at least
`drainTimeout / 1000 + 5` (a few seconds of OS buffer after the drain window).

## Cgroup-aware sizing (`workers: 0`)

Passing `workers: 0` to `serve` (or relying on the default pool size for `worker fn`
pools) makes AScript **cgroup-aware**: it reads the container's CPU quota from
`/sys/fs/cgroup/cpu.max` (cgroup v2) or `/sys/fs/cgroup/cpu/cpu.cfs_quota_us` +
`cpu.cfs_period_us` (cgroup v1) and uses `ceil(quota / period)` instead of the host's
full core count — the classic container oversubscription bug.

```js
// If the container is cpu-limited to 2 CPUs:
//   - outside a container (or no quota): workers = num_cpus (e.g. 10)
//   - inside a container with --cpus=2:  workers = 2
await srv.serve({ port: 8080, workers: 0 })
```

You can override the auto-sizing with `$ASCRIPT_WORKERS=N` (an explicit positive
integer wins unconditionally). See [Workers & parallelism → Multi-core servers](language/workers#multi-core-servers-so_reuseport).

## Detecting container context

`os.inContainer()` is a best-effort heuristic that returns `true` when the process is
running inside Docker, Podman, or Kubernetes:

```js
import * as os from "std/os"
import * as log from "std/log"

if (os.inContainer()) {
  log.info("running in container", { cpus: os.cpuCount() })
}
```

It probes `/.dockerenv`, `/run/.containerenv`, and `/proc/1/cgroup` in order. It is
ungated — succeeds even under `--sandbox`. Use it for sizing hints and log enrichment,
not security decisions. See [System & files → os.inContainer](stdlib/system#host-facts).

## Capability flags in containers

AScript's opt-out capability model applies in containers exactly as in development.
The most useful production stance is `--deny ffi` (drop the native-code gate, keep
network and filesystem):

```
CMD ["/usr/local/bin/server", "--deny", "ffi"]
```

Or use the manifest to fix the policy at build time:

```toml
# ascript.toml
[capabilities]
deny = ["ffi"]
```

`--sandbox` (deny all five capabilities) is appropriate for pure-compute workers that
should not touch the network or filesystem. See [Capabilities & sandboxing](stdlib/caps).

> [!NOTE]
> `std/docker` requires **both** `net` AND `process` — see [std/docker → Capabilities](stdlib/docker#capabilities).
> Under `--deny process` OR `--deny net` (or `--sandbox`) every `docker.*` call is
> denied.

## Containerised Docker supervisor pattern

A program that uses `std/docker` to manage sibling containers is the primary use case
for the dual-cap requirement. The `examples/advanced/docker_supervisor.as` example
shows the full pattern: connect to the daemon over UDS, watch the event stream for
`die` events, restart supervised containers under a bounded retry budget, and shut down
cleanly on `SIGTERM`.

```js
import * as docker from "std/docker"
import * as process from "std/process"

let [client, err] = await docker.connect()  // /var/run/docker.sock by default
if (err != nil) { print("docker: unavailable"); exit(0) }

// Wire SIGTERM to close the event watch and the client.
let watch = nil
process.on("SIGTERM", () => {
  if (watch != nil) { watch.close() }
  client.close()
})

let [events, watchErr] = await client.events({
  filters: { event: ["die"], label: ["app=web"] }
})
watch = events

for await (ev in events) {
  if (ev.Action != "die") { continue }
  let [_, restartErr] = await client.restart(ev.Actor.ID)
  if (restartErr != nil) { print(`restart failed: ${restartErr.message}`) }
}

events.close()
client.close()
```

See the [Docker (Engine API)](stdlib/docker) reference for the full API, and
[examples/advanced/docker_supervisor.as](examples) for the production-shaped example
with retry, log tailing, and structured logging.

## See also

- [The ascript CLI → `ascript init`](cli#ascript-init) — scaffold the server template.
- [Workers & parallelism → Multi-core servers](language/workers#multi-core-servers-so_reuseport) — `SO_REUSEPORT` accept loops across N isolates.
- [System & files → process.on](stdlib/system#processonsignalname-handler) — inbound-signal handlers.
- [Docker (Engine API)](stdlib/docker) — `std/docker` full reference.
- [Capabilities & sandboxing](stdlib/caps) — the dual-cap model.
- [Self-contained bundles](language/bundles) — `ascript build --native` + RT upgrade point.
