# Multi-stage build for an AScript server (CNTR §9.1).
#
# Stage 1 compiles the program into a single self-contained native executable
# (`ascript build --native`); stage 2 ships only that binary on a slim runtime.
#
#   docker build -t my-server .
#   docker run --rm -p 8080:8080 my-server
#
# UPGRADE POINT (§9.3): for a smaller image, swap the runtime stage for a
# distroless / scratch base. With a published `ascript-rt` base image you can
# use the runtime stub directly (RT — runtime-only native stubs — has shipped).

# ── builder ──────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS builder

# Install the AScript toolchain into the builder. Replace this with your own
# install step (download a release tarball, or COPY a vendored `ascript`).
# For example:
#   RUN curl -fsSL https://example.com/ascript-install.sh | sh
# Here we assume the toolchain binary is vendored next to the Dockerfile:
COPY ascript /usr/local/bin/ascript
RUN chmod +x /usr/local/bin/ascript

WORKDIR /src
COPY main.as ascript.toml ./
# Produce a self-contained native executable (the runtime + the program).
RUN ascript build --native main.as -o /app

# ── runtime ──────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Run as a non-root user.
RUN useradd --create-home --uid 10001 app
USER app

COPY --from=builder /app /usr/local/bin/app

# The orchestrator stops the container by sending SIGTERM; the server drains
# in-flight requests and exits 0 (see main.as `process.on("SIGTERM", ...)`).
STOPSIGNAL SIGTERM

ENV PORT=8080
EXPOSE 8080

# Liveness/readiness is via the /healthz HTTP endpoint. Configure the probe in
# your orchestrator (Kubernetes httpGet, Docker Compose healthcheck). Example
# Docker healthcheck (requires curl in the image — omitted from this slim base):
#   HEALTHCHECK CMD curl -fsS http://localhost:8080/healthz || exit 1

ENTRYPOINT ["app"]
