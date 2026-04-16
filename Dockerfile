# syntax=docker/dockerfile:1

# ── Stage 1: build the full-yolo binary ──────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}

WORKDIR /src
COPY . .
RUN cargo build --release

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    # Keep nix output readable in logs
    NIX_PAGER=cat \
    # Suppress uv's progress bars (noisy in CI / container logs)
    UV_NO_PROGRESS=1 \
    UV_SYSTEM_PYTHON=1

# ── System packages (needed before nix / uv installers run) ──────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        nodejs \
        npm \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# ── nix ───────────────────────────────────────────────────────────────────────
# DeterminateSystems installer is designed for containers: no systemd, no daemon.
# `sandbox = false` is required because Docker doesn't allow the user-namespaces
# that nix's build sandbox relies on.
RUN curl --proto '=https' --tlsv1.2 -sSf -L \
        https://install.determinate.systems/nix | \
    sh -s -- install linux \
        --no-confirm \
        --init none \
        --extra-conf "sandbox = false"

ENV PATH="/nix/var/nix/profiles/default/bin:${PATH}"

# Smoke-test nix
RUN nix --version

# ── uv ────────────────────────────────────────────────────────────────────────
RUN curl -LsSf https://astral.sh/uv/install.sh | sh
ENV PATH="/root/.local/bin:${PATH}"

# Smoke-test uv
RUN uv --version

# ── claude CLI ────────────────────────────────────────────────────────────────
RUN npm install -g @anthropic-ai/claude-code

# ── full-yolo binary ──────────────────────────────────────────────────────────
COPY --from=builder /src/target/release/full-yolo /usr/local/bin/full-yolo

# /workspace is mounted from the host — this is where claude reads/writes code
WORKDIR /workspace
VOLUME ["/workspace"]

# Default: run full-yolo in the mounted workspace.
# The user passes task / prompt flags after the image name, e.g.:
#   docker run ... ghcr.io/… -t "build a todo app" -p owner/repo
ENTRYPOINT ["full-yolo", "--work-dir", "/workspace"]
