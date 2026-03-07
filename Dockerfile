# syntax=docker/dockerfile:1

FROM rust:1.94.0-bookworm AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM mcr.microsoft.com/playwright:v1.51.0-jammy AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 -s /bin/bash user

WORKDIR /home/user
COPY --from=builder /app/target/release/openclaw-entrypoint /usr/local/bin/openclaw-entrypoint

USER 1000:1000
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openclaw-entrypoint"]
CMD ["bash"]
