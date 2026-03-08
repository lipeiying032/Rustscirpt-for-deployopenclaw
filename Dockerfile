# syntax=docker/dockerfile:1

# ── Stage 1: Compile the Rust sync helper ─────────────────────────────────────
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
COPY src ./src
# Do not copy Cargo.lock — let Cargo resolve a fresh consistent lockfile.
# (Our hand-edited Cargo.lock drifted from Cargo.toml, causing --locked to fail.)
RUN cargo build --release

# ── Stage 2: Runtime ───────────────────────────────────────────────────────────
# Playwright jammy ships Chromium (required by OpenClaw's browser-control).
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

USER root

# ── 2a. System deps ────────────────────────────────────────────────────────────
# git      : required by openclaw npm install (avoids "spawn git ENOENT")
# cmake/make/python3/build-essential : openclaw native deps (canvas, sharp, etc.)
# python3-venv : isolated LiteLLM install to avoid externally-managed-env errors
# tini     : proper pid-1 signal forwarding
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git tini ca-certificates curl \
        cmake make build-essential \
        python3 python3-venv \
    && rm -rf /var/lib/apt/lists/*

# ── 2b. Install Node 22 (OpenClaw requires ≥22; Playwright ships Node 20) ─────
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# ── 2c. Install OpenClaw globally ─────────────────────────────────────────────
# SHARP_IGNORE_GLOBAL_LIBVIPS=1 : skip system libvips check (avoids build fail)
# npm_config_cache=/tmp/npm-cache : writable cache dir during build
RUN SHARP_IGNORE_GLOBAL_LIBVIPS=1 \
    npm_config_cache=/tmp/npm-cache \
    npm install -g openclaw@latest \
    && rm -rf /tmp/npm-cache

# ── 2d. Install LiteLLM into an isolated venv ─────────────────────────────────
RUN python3 -m venv /opt/litellm-venv \
    && /opt/litellm-venv/bin/pip install --no-cache-dir "litellm[proxy]"

# Make litellm available on PATH
ENV PATH="/opt/litellm-venv/bin:$PATH"

# ── 2e. Copy Rust sync binary + startup script ────────────────────────────────
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
COPY start.sh /app/start.sh
RUN chmod +x /usr/local/bin/openclaw-hf-sync /app/start.sh

# ── 2f. Create runtime user (uid 1000) and openclaw data dir ──────────────────
RUN set -eux; \
    if ! getent passwd 1000 >/dev/null; then \
        groupadd -g 1000 user; \
        useradd -m -u 1000 -g 1000 -s /bin/bash user; \
    fi; \
    mkdir -p /home/user/.openclaw /home/user/app; \
    chown -R 1000:1000 /home/user

# ── Port config ────────────────────────────────────────────────────────────────
# HF Space health-check uses port 7860.
# OPENCLAW_API_PORT overrides OpenClaw's default (18789).
# LiteLLM proxy listens on 127.0.0.1:4000 (internal only).
ENV OPENCLAW_API_PORT=7860 \
    OPENCLAW_WS_PORT=7861 \
    HOME=/home/user \
    SHARP_IGNORE_GLOBAL_LIBVIPS=1

EXPOSE 7860 7861

WORKDIR /home/user/app
USER 1000:1000

# ── Entrypoint ─────────────────────────────────────────────────────────────────
# openclaw-hf-sync (pid 1 via tini):
#   1. Pulls ~/.openclaw from the HF dataset
#   2. Spawns start.sh → LiteLLM proxy + OpenClaw gateway
#   3. Pushes workspace changes back on a timer and on shutdown
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
CMD ["/app/start.sh"]