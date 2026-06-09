# Multi-stage build: compile Rust binary for Linux, then run on minimal image.

# ── Stage 1: Build ──────────────────────────────────────────
FROM rust:1.96-slim AS builder

# portable-pty needs a C compiler for some sys dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build release binary
RUN cargo build --release --bin agentbox-wrapper

# ── Stage 2: Runtime ────────────────────────────────────────
FROM ubuntu:24.04

# System deps
RUN apt-get update && apt-get install -y \
    ca-certificates curl git jq \
    && rm -rf /var/lib/apt/lists/*

# Node.js (for MCP servers that agents may use)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs

# Antigravity CLI
RUN curl -fsSL https://antigravity.google/cli/install.sh | bash

# GitHub Copilot CLI (for copilot driver)
RUN npm install -g @github/copilot && npm cache clean --force

# Config directory
RUN mkdir -p /root/.gemini/antigravity-cli
ENV PATH="/root/.local/bin:${PATH}"

# Copy compiled binary and entrypoint from build context
COPY --from=builder /build/target/release/agentbox-wrapper /usr/local/bin/agentbox-wrapper
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/agentbox-wrapper /usr/local/bin/entrypoint.sh

WORKDIR /workspace
EXPOSE 7080

ENTRYPOINT ["entrypoint.sh"]
