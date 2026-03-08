# syntax=docker/dockerfile:1

# ── Stage 1: Compile the Rust sync helper ─────────────────────────────────────
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
COPY Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
# Base: OpenClaw official image (Node 22 + openclaw.mjs)
FROM ghcr.io/openclaw/openclaw:latest AS runtime

USER root

# Install: tini (pid1), curl (health-check), Python 3 + pip (LiteLLM)
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        tini ca-certificates curl \
        python3 python3-pip \
    && pip3 install --no-cache-dir --break-system-packages litellm[proxy] \
    && rm -rf /var/lib/apt/lists/*

# Copy Rust sync binary
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
RUN chmod +x /usr/local/bin/openclaw-hf-sync

# Copy startup script
COPY start.sh /app/start.sh
RUN chmod +x /app/start.sh

# Ensure OpenClaw data dir belongs to the node user (uid 1000)
RUN mkdir -p /home/node/.openclaw && chown -R node:node /home/node/.openclaw

# ── Port config ───────────────────────────────────────────────────────────────
# HF Space health-check expects port 7860.
# OpenClaw listens on OPENCLAW_API_PORT; LiteLLM proxy on 4000 (internal only).
ENV OPENCLAW_API_PORT=7860 \
    OPENCLAW_WS_PORT=7861 \
    HOME=/home/node

EXPOSE 7860 7861

WORKDIR /app
USER node

# ── Entrypoint ────────────────────────────────────────────────────────────────
# openclaw-hf-sync:
#   1. Pulls ~/.openclaw workspace from the HF dataset
#   2. Spawns start.sh (LiteLLM proxy → OpenClaw gateway)
#   3. Periodically pushes workspace changes back, and on shutdown
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
CMD ["/app/start.sh"]