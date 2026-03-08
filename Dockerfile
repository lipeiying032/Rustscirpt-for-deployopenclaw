# syntax=docker/dockerfile:1

FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
COPY Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    if ! getent passwd 1000 >/dev/null; then \
        groupadd -g 1000 user; \
        useradd -m -u 1000 -g 1000 -s /bin/bash user; \
    fi; \
    mkdir -p /home/user/.openclaw/workspace /home/user/app; \
    chown -R 1000:1000 /home/user

COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
RUN chmod +x /usr/local/bin/openclaw-hf-sync

ENV HOME=/home/user
WORKDIR /home/user/app
USER 1000:1000

EXPOSE 7860

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
# Replace the CMD below with your actual service command, e.g.:
#   CMD ["node", "server.js"]
#   CMD ["python", "app.py"]
CMD ["node", "server.js"]