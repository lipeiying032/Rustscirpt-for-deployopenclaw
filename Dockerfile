# syntax=docker/dockerfile:1

FROM rust:1.94.0-slim AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM node:20-bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

# 1. 安装系统底层依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl unzip libssl3 \
    libnss3 libatk-bridge2.0-0 libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 \
    libxrandr2 libgbm1 libasound2 libatk1.0-0 libcups2 libdrm2 libdbus-1-3 libgtk-3-0 \
    libx11-xcb1 libxshmfence1 libxext6 libx11-6 fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

# 2. 兼容性处理用户（避免 UID 1000 冲突导致 exit code 4）
RUN id -u user >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash user

# 3. 准备目录并设置权限
RUN mkdir -p /home/user/.openclaw/workspace /home/user/app /home/user/.cache \
    && chown -R 1000:1000 /home/user

# 4. 拷贝 Rust 编译产物
COPY --from=builder /build/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
RUN chmod +x /usr/local/bin/openclaw-hf-sync

# 5. 切换到用户权限后安装 Playwright
USER user
WORKDIR /home/user/app

ENV HOME=/home/user
ENV PLAYWRIGHT_BROWSERS_PATH=/home/user/.cache/ms-playwright
ENV PATH="/home/user/.npm-global/bin:${PATH}"

RUN npx playwright install chromium

ENTRYPOINT ["/usr/local/bin/openclaw-hf-sync"]
