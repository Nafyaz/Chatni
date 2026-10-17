# Stage 1: Build the Rust binary
FROM rust:1.82-bookworm AS builder
WORKDIR /app

COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

# Stage 2: Grab the official GitHub MCP Server binary (optional local stdio support)
FROM ghcr.io/github/github-mcp-server:latest AS github-mcp

# Stage 3: Minimal runtime image
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/chatni /usr/local/bin/chatni
COPY --from=github-mcp /server/github-mcp-server /usr/local/bin/github-mcp-server
COPY mcp.json /app/mcp.json

ENV HOST=0.0.0.0
ENV PORT=8080
ENV SQLITE_PATH=/data/chatni.db
VOLUME ["/data"]

EXPOSE 8080
CMD ["chatni"]
