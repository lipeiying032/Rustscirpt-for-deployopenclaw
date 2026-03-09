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
RUN cargo build --release

# ── Stage 2: Runtime ───────────────────────────────────────────────────────────
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

USER root

# ── 2a. System deps ────────────────────────────────────────────────────────────
# NOTE: python3-venv is intentionally omitted.
# Ubuntu jammy security mirror has removed python3.10-venv (404), which makes
# dpkg fail at configure time even with --fix-missing. We use pip directly instead.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git tini ca-certificates curl \
        cmake make build-essential \
        python3 python3-pip \
    && rm -rf /var/lib/apt/lists/*

# ── 2b. Install Node 22 (OpenClaw requires ≥22; Playwright ships Node 20) ─────
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# ── 2c. Install OpenClaw globally ─────────────────────────────────────────────
RUN SHARP_IGNORE_GLOBAL_LIBVIPS=1 \
    npm_config_cache=/tmp/npm-cache \
    npm install -g openclaw@latest \
    && rm -rf /tmp/npm-cache

# ── 2d. Install LiteLLM via pip (no venv — jammy's python3.10-venv is broken) ─
RUN pip3 install --no-cache-dir --break-system-packages "litellm[proxy]"

# ── 2e. Copy Rust sync binary + startup script ────────────────────────────────
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
COPY start.sh /app/start.sh
RUN chmod +x /usr/local/bin/openclaw-hf-sync /app/start.sh

# ── 2f. Create runtime user (uid 1000) ────────────────────────────────────────
RUN set -eux; \
    if ! getent passwd 1000 >/dev/null; then \
        groupadd -g 1000 user; \
        useradd -m -u 1000 -g 1000 -s /bin/bash user; \
    fi; \
    mkdir -p /home/user/.openclaw /home/user/app; \
    chown -R 1000:1000 /home/user

ENV OPENCLAW_API_PORT=7860 \
    OPENCLAW_WS_PORT=7861 \
    HOME=/home/user \
    SHARP_IGNORE_GLOBAL_LIBVIPS=1

EXPOSE 7860 7861

WORKDIR /home/user/app
USER 1000:1000

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
CMD ["/app/start.sh"]