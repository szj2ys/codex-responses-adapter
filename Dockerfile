# ---------- Build stage ----------
ARG REGISTRY=docker.io/library
FROM ${REGISTRY}/rust:1.85-slim-bookworm AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release 2>/dev/null || true

COPY . .
RUN touch src/main.rs
RUN cargo build --release

# ---------- Runtime stage ----------
ARG REGISTRY=docker.io/library
FROM ${REGISTRY}/debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/codex-responses-adapter /usr/local/bin/codex-responses-adapter

EXPOSE 6789

ENTRYPOINT ["codex-responses-adapter"]
