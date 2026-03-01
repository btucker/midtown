# Dockerfile — Production image for running Midtown
#
# Multi-stage build: compile with Rust toolchain, ship only the binary
# with runtime dependencies (Zellij, git, gh CLI).
#
# Usage:
#   docker build -t midtown .
#   docker run -it --rm -v /path/to/repo:/repo -w /repo midtown start
#
# For persistent state, mount the midtown config directory:
#   docker run -it --rm \
#     -v ~/.midtown:/home/midtown/.midtown \
#     -v /path/to/repo:/repo \
#     -w /repo midtown start

# syntax=docker/dockerfile:1

# ─── Stage 1: Chef planner ──────────────────────────────────────────────────
# Analyzes the project and creates a "recipe" of dependencies
FROM rust:bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

# ─── Stage 2: Recipe creation ───────────────────────────────────────────────
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ─── Stage 3: Web-app build ───────────────────────────────────────────────
FROM node:22-slim AS web-builder
WORKDIR /app/web-app
COPY web-app/ .
RUN npm ci && npm run build

# ─── Stage 4: Rust build ─────────────────────────────────────────────────────
FROM chef AS builder

# System dependencies for building
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Build dependencies from recipe (cached layer)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Now copy source and build the actual application
COPY . .
RUN cargo build --release

# ─── Stage 5: Runtime ───────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Runtime dependencies:
# - git: worktree management
# - curl: general use, Claude CLI install
# - procps: process inspection (ps)
# - jq: JSON parsing in scripts
# - ca-certificates: HTTPS connections
# Note: Zellij is installed separately below (not available via apt)
RUN apt-get update && apt-get install -y --no-install-recommends \
        git \
        curl \
        unzip \
        procps \
        jq \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# gh CLI via GitHub's apt repo
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
        | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y gh \
    && rm -rf /var/lib/apt/lists/*

# Zellij terminal multiplexer (installed from GitHub releases)
RUN ARCH=$(dpkg --print-architecture) \
    && if [ "$ARCH" = "amd64" ]; then ZELLIJ_ARCH="x86_64"; else ZELLIJ_ARCH="aarch64"; fi \
    && curl -fsSL "https://github.com/zellij-org/zellij/releases/latest/download/zellij-${ZELLIJ_ARCH}-unknown-linux-musl.tar.gz" \
        | tar xzf - -C /usr/local/bin/ \
    && chmod +x /usr/local/bin/zellij

# Non-root user for security and proper $HOME handling
RUN useradd -m -s /bin/bash midtown

# Install CLI tools as midtown user BEFORE copying the binary so these
# layers are cached even when source code changes.
USER midtown
WORKDIR /home/midtown

# Bun (JavaScript runtime, also used for npm packages)
RUN curl -fsSL https://bun.sh/install | bash \
    || echo "Bun install failed (may not be available in all environments)"

ENV PATH="/home/midtown/.bun/bin:${PATH}"

# Claude Code CLI
RUN curl -fsSL https://claude.ai/install.sh | bash \
    && echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc \
    || echo "Claude CLI install failed (may not be available in all environments)"

ENV PATH="/home/midtown/.local/bin:${PATH}"

# OpenAI Codex CLI
RUN bun install -g @openai/codex \
    || echo "Codex CLI install failed (may not be available in all environments)"

# Default git config (can be overridden via environment)
RUN git config --global user.email "midtown@docker.local" \
    && git config --global user.name "Midtown Docker" \
    && git config --global init.defaultBranch main

# Copy the binary from builder and web-app from web-builder LAST so code
# changes don't invalidate the CLI install layers above.
USER root
COPY --from=builder /app/target/release/midtown /usr/local/bin/midtown
COPY --from=web-builder /app/web-app/dist /usr/local/bin/web-app/dist
USER midtown

ENTRYPOINT ["midtown"]
CMD ["--help"]
