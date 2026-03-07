# syntax=docker/dockerfile:1

# --- 阶段 1: 编译 ---
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

# 关键：除了 pkg-config，还必须有 libssl-dev 来支持 reqwest 的 tls 功能
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
# 如果你有 Cargo.lock，请取消下面一行的注释
# COPY Cargo.lock ./ 

COPY src ./src

# 执行编译
RUN cargo build --release

# --- 阶段 2: 运行 ---
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

# 安装运行时必备库
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tini \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# 解决 Hugging Face UID 冲突
RUN id -u user >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash user

WORKDIR /home/user/app

# 【自动识别拷贝】不再手动写死文件名，防止写错
# 这行命令会把 target/release 下所有的可执行文件拷贝到 bin 目录
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/

# 确保权限和目录结构
RUN chmod +x /usr/local/bin/openclaw-hf-sync && \
    mkdir -p /home/user/.openclaw/workspace && \
    chown -R 1000:1000 /home/user

ENV HOME=/home/user
USER 1000:1000

# 确保 ENTRYPOINT 指向正确的文件名
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
