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

# ── Stage 2: Runtime — use OpenClaw's official image (Node 22 + openclaw.mjs) ─
FROM ghcr.io/openclaw/openclaw:latest AS runtime

# Switch to root just long enough to install tini and copy our binary
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends tini ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
RUN chmod +x /usr/local/bin/openclaw-hf-sync

# Ensure the openclaw data dir exists and is owned by the node user (uid 1000)
RUN mkdir -p /home/node/.openclaw && chown -R node:node /home/node/.openclaw

# ── OpenClaw port config ───────────────────────────────────────────────────────
# HF Space expects the app to listen on 7860 (declared in README.md app_port).
# We override OpenClaw's default (18789) so the Space health-check passes.
ENV OPENCLAW_API_PORT=7860 \
    OPENCLAW_WS_PORT=7861 \
    HOME=/home/node

EXPOSE 7860 7861

WORKDIR /app
USER node

# ── Entrypoint ─────────────────────────────────────────────────────────────────
# openclaw-hf-sync wraps the child process:
#   1. pull ~/.openclaw from the HF dataset
#   2. spawn the OpenClaw gateway (arguments after --)
#   3. push changes back on a timer and on shutdown
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
CMD ["node", "openclaw.mjs", "gateway", "--allow-unconfigured"]