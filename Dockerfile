# syntax=docker/dockerfile:1

# --- 阶段 1: 编译 Rust 二进制文件 ---
FROM rust:1.85-slim-bookworm AS builder
WORKDIR /app

# 安装编译所需的系统依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# 拷贝依赖配置
COPY Cargo.toml ./

# 拷贝源代码并进行编译
COPY src ./src
RUN cargo build --release

# --- 阶段 2: 最终运行环境 ---
# 使用 Playwright 官方镜像确保浏览器环境完整
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

# 安装运行时工具
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tini \
    && rm -rf /var/lib/apt/lists/*

# 解决 UID 1000 冲突（Hugging Face 必加）
RUN id -u user >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash user

# 准备工作目录
WORKDIR /home/user/app

# 【核心修复】从 builder 拷贝正确的二进制文件名
# 路径必须与 Cargo.toml 中的 name = "openclaw-hf-sync" 对应
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync

# 设置权限
RUN chmod +x /usr/local/bin/openclaw-hf-sync && \
    mkdir -p /home/user/.openclaw/workspace && \
    chown -R 1000:1000 /home/user

# 设置环境变量
ENV HOME=/home/user
USER 1000:1000

# 使用 tini 启动
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
