# syntax=docker/dockerfile:1

# --- 阶段 1: 编译阶段 ---
# 使用 1.94.0 确保支持最新的异步语法
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

# 安装编译所需的底层工具
# pkg-config 和 libssl-dev 是 reqwest (tls) 必须的
# build-essential 是编译 nix 等原生 C 绑定库必须的
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# 拷贝并编译
COPY Cargo.toml ./
# 如果你有 Cargo.lock 请务必加上这一行: COPY Cargo.lock ./
COPY src ./src

# 编译二进制文件
RUN cargo build --release

# --- 阶段 2: 运行阶段 ---
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

# 安装运行时必要的库（libssl3 是 reqwest 运行必须的）
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tini \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# 处理 UID 1000 冲突：将 Playwright 镜像默认的 pwuser 改名为 user
RUN EXISTING_USER=$(id -nu 1000) && \
    if [ "$EXISTING_USER" != "user" ]; then \
        usermod -l user $EXISTING_USER && \
        groupmod -n user $EXISTING_USER && \
        usermod -d /home/user -m user; \
    fi || (useradd -m -u 1000 -s /bin/bash user || true)

# 准备代码中定义的 WORKSPACE 目录
RUN mkdir -p /home/user/.openclaw/workspace /home/user/app && \
    chown -R 1000:1000 /home/user

WORKDIR /home/user/app

# 拷贝编译好的二进制文件
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/openclaw-hf-sync
RUN chmod +x /usr/local/bin/openclaw-hf-sync

# 设置环境变量
ENV HOME=/home/user
USER 1000:1000

# 启动逻辑：使用 tini 启动你的同步程序
# 注意：你需要通过 CMD 传入你想要运行的实际爬虫或程序名
# 例如：ENTRYPOINT + ["python", "app.py"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]

# 默认命令（如果没有提供参数，则保持容器不退出）
CMD ["bash"]
