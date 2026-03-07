# syntax=docker/dockerfile:1

FROM rust:1.75-slim AS builder
WORKDIR /build

COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM node:20-bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl unzip \
    libnss3 libatk-bridge2.0-0 libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 \
    libxrandr2 libgbm1 libasound2 libatk1.0-0 libcups2 libdrm2 libdbus-1-3 libgtk-3-0 \
    libx11-xcb1 libxshmfence1 libxext6 libx11-6 fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 -s /bin/bash user
USER user
WORKDIR /home/user/app

ENV HOME=/home/user
ENV PLAYWRIGHT_BROWSERS_PATH=/home/user/.cache/ms-playwright
RUN npx playwright install chromium

USER root
COPY --from=builder /build/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
RUN chmod +x /usr/local/bin/openclaw-hf-sync \
    && mkdir -p /home/user/.openclaw/workspace /home/user/app \
    && chown -R user:user /home/user

USER user
WORKDIR /home/user/app
ENTRYPOINT ["/usr/local/bin/openclaw-hf-sync"]