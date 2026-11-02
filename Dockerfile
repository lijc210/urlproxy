# syntax=docker/dockerfile:1

# ---- build stage ----
FROM rust:1.91-slim-bookworm AS builder

WORKDIR /app

# 先只拷贝依赖清单，用一个空的 main.rs 把依赖编译一遍。
# 这样业务代码变动时只需要重新编译本项目的 crate，依赖层可以命中构建缓存。
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked

COPY src ./src
COPY assets ./assets
# COPY 会保留原文件时间戳，touch 一下强制 cargo 重新编译真实源码。
RUN touch src/main.rs && cargo build --release --locked

# ---- runtime stage ----
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --no-create-home urlproxy

COPY --from=builder /app/target/release/urlproxy /usr/local/bin/urlproxy

ENV ADDR=0.0.0.0:3000 \
    RUST_LOG=urlproxy=info,tower_http=warn

USER urlproxy
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD ["curl", "-fsS", "http://127.0.0.1:3000/healthz"]

ENTRYPOINT ["/usr/local/bin/urlproxy"]
