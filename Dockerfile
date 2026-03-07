# syntax=docker/dockerfile:1

# --- 阶段 1: 编译 Rust 二进制文件 ---
FROM rust:1.94.0-slim-bookworm AS builder
WORKDIR /app

# 安装编译所需的系统依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# 拷贝配置文件并预编译依赖（利用 Docker 缓存）
COPY Cargo.toml ./
# 如果有 Cargo.lock 也建议加上: COPY Cargo.lock ./ 

# 拷贝源代码并进行正式编译
COPY src ./src
RUN cargo build --release

# --- 阶段 2: 最终运行环境 ---
# 使用 Playwright 官方镜像，注意：此镜像基于 Ubuntu，
# 但为了确保浏览器依赖完整，它是最稳妥的选择。
FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

# 安装额外的运行时工具（如 tini 用于进程管理）
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tini \
    && rm -rf /var/lib/apt/lists/*

# 【关键修改】修复 exit code 4: 兼容性处理用户创建
# 如果 UID 1000 或 user 已存在则跳过，否则创建它
RUN id -u user >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash user

# 准备工作目录并确保权限正确
WORKDIR /home/user/app
RUN chown -R 1000:1000 /home/user

# 从 builder 阶段拷贝编译好的二进制文件
# 注意：请确认您的二进制文件名是 openclaw-entrypoint 还是 openclaw-hf-sync
COPY --from=builder /app/target/release/openclaw-entrypoint /usr/local/bin/openclaw-entrypoint
RUN chmod +x /usr/local/bin/openclaw-entrypoint

# 设置环境变量，确保 Playwright 能找到浏览器
ENV HOME=/home/user
ENV PATH="/home/user/.local/bin:${PATH}"

# 切换到非 root 用户运行
USER 1000:1000

# 使用 tini 作为 init 进程，防止僵尸进程并正确处理信号
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-entrypoint"]

# 默认启动命令
CMD ["bash"]
