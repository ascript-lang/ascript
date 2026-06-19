# Repo-level multi-stage Dockerfile (CNTR §9.1).
#
# Builds the AScript toolchain from source, then compiles an example program into
# a single self-contained native binary and ships it on a slim, non-root runtime.
# This is the reference image the §9.3 `ascript init --template server` scaffold
# mirrors; CI builds it against the repo's examples.
#
# Build from the repo root:
#   docker build -f docker/Dockerfile -t ascript-server .
#   docker run --rm -p 8080:8080 ascript-server
#
# Override the program with a build arg:
#   docker build -f docker/Dockerfile --build-arg APP=examples/advanced/http_server.as -t app .
#
# UPGRADE POINT (§9.3): swap the runtime stage for a distroless / scratch base, or
# use a published `ascript-rt` runtime-stub base image (RT — runtime-only native
# stubs — has shipped; `ascript build --native --oci` produces a Docker-less image).

# ── toolchain builder ────────────────────────────────────────────────────────
FROM rust:1-bookworm AS toolchain
WORKDIR /ascript
COPY . .
# Build the release toolchain binary (default features = full stdlib).
RUN cargo build --release --bin ascript

# ── app builder ──────────────────────────────────────────────────────────────
FROM toolchain AS appbuild
ARG APP=templates/server/main.as
# Produce a self-contained native executable (the runtime + the program).
RUN ./target/release/ascript build --native "${APP}" -o /app

# ── runtime ──────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN useradd --create-home --uid 10001 app
USER app
COPY --from=appbuild /app /usr/local/bin/app

# The orchestrator stops the container by sending SIGTERM; the server drains
# in-flight requests and exits 0.
STOPSIGNAL SIGTERM

ENV PORT=8080
EXPOSE 8080

# Liveness/readiness is via the /healthz HTTP endpoint — configure the probe in
# your orchestrator (Kubernetes httpGet, Docker Compose healthcheck). There is no
# runtime `--health` flag.

ENTRYPOINT ["app"]
