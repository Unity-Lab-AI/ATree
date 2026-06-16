# Multi-stage build for ATree — production deployment
# Stage 1: Build
FROM rust:1.85-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY atree-engine/Cargo.toml atree-engine/Cargo.toml
COPY atree-cli/Cargo.toml atree-cli/Cargo.toml
COPY atree-web/Cargo.toml atree-web/Cargo.toml

# Build dependencies cache layer
RUN mkdir -p atree-engine/src atree-cli/src atree-web/src && \
    echo 'fn main() {}' > atree-cli/src/main.rs && \
    echo '' > atree-engine/src/lib.rs && \
    echo '' > atree-web/src/lib.rs && \
    cargo build --release 2>/dev/null || true

# Build actual binaries
COPY . .
RUN cargo build --release --bin atree && \
    cargo build --release --bin atree-web

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

RUN useradd --create-home atree

WORKDIR /app
COPY --from=builder /build/target/release/atree /usr/local/bin/atree
COPY --from=builder /build/target/release/atree-web /usr/local/bin/atree-web
COPY --from=builder /build/atree-web/static /app/atree-web/static

RUN mkdir -p /data && chown atree:atree /data
USER atree

VOLUME ["/data"]

# atree-web: serve the visual graph
EXPOSE 3020
ENV ATREE_DB_PATH=/data/index.sqlite

# Default: run the web server
CMD ["atree-web", "--db", "/data/index.sqlite", "--port", "3020"]
