# syntax=docker/dockerfile:1
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev build-essential && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml ./
COPY src ./src
# 这里的编译如果报错，请重点看 main.rs 的第 100-120 行左右
RUN cargo build --release

FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini libssl3 && rm -rf /var/lib/apt/lists/*

# 处理 UID 1000 冲突：直接重命名现有的 pwuser 为 user
RUN EXISTING_USER=$(id -nu 1000) && \
    if [ "$EXISTING_USER" != "user" ]; then \
        usermod -l user $EXISTING_USER && \
        groupmod -n user $EXISTING_USER && \
        usermod -d /home/user -m user; \
    fi || useradd -m -u 1000 -s /bin/bash user

WORKDIR /home/user/app
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/
RUN chmod +x /usr/local/bin/openclaw-hf-sync && \
    mkdir -p /home/user/.openclaw/workspace && \
    chown -R 1000:1000 /home/user

ENV HOME=/home/user
USER 1000:1000
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
