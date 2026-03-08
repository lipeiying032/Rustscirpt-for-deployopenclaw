# syntax=docker/dockerfile:1

# ── Stage 1: Compile the Rust sync helper ─────────────────────────────────────
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
COPY Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# ── Stage 2: Runtime ───────────────────────────────────────────────────────────
# Use the official Playwright image — it has Chromium (required by OpenClaw's
# browser-control features) and a compatible Node.js version pre-installed.
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

USER root

# ── 2a. Install Node 22 (Playwright image ships Node 20; OpenClaw requires ≥22)
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# ── 2b. Install build tools required by openclaw's native deps + tini + Python
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        tini \
        make cmake build-essential python3 python3-pip \
    && rm -rf /var/lib/apt/lists/*

# ── 2c. Install OpenClaw globally (provides openclaw.mjs)
RUN npm install -g openclaw@latest

# ── 2d. Install LiteLLM proxy
RUN pip3 install --no-cache-dir --break-system-packages "litellm[proxy]"

# ── 2e. Copy Rust sync binary + startup script
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
COPY start.sh /app/start.sh
RUN chmod +x /usr/local/bin/openclaw-hf-sync /app/start.sh

# ── 2f. Ensure OpenClaw data dir belongs to the runtime user (uid 1000)
RUN set -eux; \
    if ! getent passwd 1000 >/dev/null; then \
        groupadd -g 1000 user; \
        useradd -m -u 1000 -g 1000 -s /bin/bash user; \
    fi; \
    mkdir -p /home/user/.openclaw /home/user/app; \
    chown -R 1000:1000 /home/user

# ── Port config ────────────────────────────────────────────────────────────────
# HF Space health-check expects port 7860.
# OpenClaw listens on OPENCLAW_API_PORT; LiteLLM Proxy on 4000 (internal only).
ENV OPENCLAW_API_PORT=7860 \
    OPENCLAW_WS_PORT=7861 \
    HOME=/home/user

EXPOSE 7860 7861

WORKDIR /home/user/app
USER 1000:1000

# ── Entrypoint ─────────────────────────────────────────────────────────────────
# openclaw-hf-sync:
#   1. Pulls ~/.openclaw workspace from the HF dataset
#   2. Spawns start.sh  →  LiteLLM proxy (127.0.0.1:4000) + OpenClaw gateway
#   3. Periodically pushes workspace changes back to HF dataset, and on shutdown
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
CMD ["/app/start.sh"]