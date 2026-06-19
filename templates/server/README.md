# AScript server

A minimal, production-shaped HTTP service scaffolded by `ascript init --template server`:
a `/healthz` liveness probe, a root route, a resilient upstream-proxy route, and a clean
SIGTERM/SIGINT drain so orchestrators can stop the container gracefully.

## Run locally

```sh
ascript run main.as              # binds 0.0.0.0:8080 (override with PORT / HOST)
PORT=3000 ascript run main.as    # bind a different port
curl localhost:8080/healthz      # {"ok":true,"uptimeMs":...}
curl localhost:8080/             # hello from ascript
curl localhost:8080/proxy        # the retry-wrapped upstream call
```

Stop it with Ctrl-C (SIGINT) or `kill -TERM <pid>`: in-flight requests drain (up to
`drainTimeout` ms), then the process exits 0. You'll see `drain started` then `stopped`.

## Build a single binary

```sh
ascript build --native main.as -o app
./app
```

`build --native` produces one self-contained executable (the runtime + your program). No
AScript install is needed on the target host.

## Containerize

The included `Dockerfile` is a multi-stage build: stage 1 compiles `main.as` to a native
binary, stage 2 ships it on a slim, non-root runtime with `STOPSIGNAL SIGTERM`.

```sh
docker build -t my-server .
docker run --rm -p 8080:8080 my-server
```

The build context assumes the `ascript` toolchain binary is vendored next to the
`Dockerfile` (the `COPY ascript /usr/local/bin/ascript` line) — replace that step with your
preferred install (a release tarball, a base image, etc.).

Health checks use the `/healthz` HTTP endpoint — wire it into your orchestrator
(Kubernetes `httpGet`, Compose `healthcheck`). There is no `--health` runtime flag.

## Configuration

| Env var | Default     | Purpose                |
| ------- | ----------- | ---------------------- |
| `PORT`  | `8080`      | listen port            |
| `HOST`  | `0.0.0.0`   | listen address         |

## Upgrade points (§9.3)

These are the marked extension seams in `main.as` — all of the referenced features have
already shipped, so they are enabled follow-ups, not blocked work:

- **Resilience policies.** Swap the hand-rolled `task.retry` in the `/proxy` route for a
  composed `std/resilience` policy (deadline → breaker → retry, per-client rate limits).
  See `examples/advanced/resilient_gateway.as`.
- **Smaller images.** Swap the runtime stage for a distroless / scratch base, or use a
  published `ascript-rt` runtime-stub base image (RT has shipped — `ascript build --native`
  already resolves a minimal runtime stub; `--oci` produces a Docker-less scratch image).
