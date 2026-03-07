# syntax=docker/dockerfile:1

FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

# 安装完整编译工具链
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev build-essential \
    && rm -rf /var/lib/apt/lists/*

# 1. 先拷贝 Cargo.toml 
COPY Cargo.toml ./
# 如果你有 Cargo.lock，取消下面一行的注释（这能极大提高构建成功率）
# COPY Cargo.lock ./ 

# 2. 拷贝代码
COPY src ./src

# 3. 编译并显示详细错误（如果失败，请查看 Build Logs 里的红色文字）
RUN cargo build --release || (echo "编译失败！请检查下方代码错误：" && cargo build --release --color never)

# --- 运行阶段 ---
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates tini libssl3 \
    && rm -rf /var/lib/apt/lists/*

# 兼容性处理用户
RUN id -u user >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash user

WORKDIR /home/user/app

# 拷贝二进制文件
COPY --from=builder /app/target/release/openclaw-hf-sync /usr/local/bin/

# 权限设置
RUN chmod +x /usr/local/bin/openclaw-hf-sync && \
    mkdir -p /home/user/.openclaw/workspace && \
    chown -R 1000:1000 /home/user

ENV HOME=/home/user
USER 1000:1000

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-hf-sync"]
