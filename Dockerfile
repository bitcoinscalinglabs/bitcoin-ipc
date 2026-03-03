# This Dockerfile sets up:
# 1. Bitcoin Core with regtest configuration
# 2. IPC repo (bitcoinscalinglabs/ipc) with all dependencies
# 3. Bitcoin-IPC repo
# 4. Wallet and IPC configuration (using src/bin/quickstart)

# Stage 1: Build environment for Bitcoin-IPC
FROM rust:1.87.0-slim AS bitcoin-ipc-builder

ARG EMISSION_CHAIN_FEATURES=false

# Install build dependencies
RUN apt-get update && apt-get install -y \
    git \
    curl \
    pkg-config \
    libssl-dev \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /workspace

# Copy local bitcoin-ipc repo from build context
WORKDIR /workspace/bitcoin-ipc
COPY . /workspace/bitcoin-ipc

# Build bitcoin-ipc repo
RUN if [ "$EMISSION_CHAIN_FEATURES" = "true" ]; then \
        cargo build --release --features emission_chain; \
    else \
        cargo build --release; \
    fi

# Stage 2: IPC artifacts come from a prebuilt image (build once and reuse):
# docker build -f docker-deploy-local/Dockerfile.ipc -t ipc-builder:latest .
FROM ipc-builder:latest AS ipc-builder

# Stage 3: Final runtime environment
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    python3-pip \
    build-essential \
    ca-certificates \
    libssl3 \
    curl \
    wget \
    gnupg \
    jq \
    git \
    make \
    procps \
    && rm -rf /var/lib/apt/lists/*

# Install Node.js (includes npm). Needed by IPC contracts build which runs as part of
# `make docker-build` for fendermint in the container entrypoint.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get update && \
    apt-get install -y nodejs && \
    rm -rf /var/lib/apt/lists/* && \
    node --version && \
    npm --version && \
    npm install -g pnpm && \
    pnpm --version

# Install a modern Docker CLI (client-only).
#
# Why:
# - We mount the host Docker socket (`/var/run/docker.sock`) into this container so we can
#   build and run host-managed containers (e.g. fendermint) from inside this container.
# - Debian's `docker.io` package can be too old for Docker Desktop / newer daemons, which
#   causes `docker info`/`docker run` to fail.
# - `docker-ce-cli` is distributed via Docker's official APT repository.
# - APT verifies package signatures using Docker's public signing key.
# - We store it in a repo-specific keyring (preferred over deprecated `apt-key`).
RUN set -e; \
    install -m 0755 -d /etc/apt/keyrings; \
    curl -fsSL https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg; \
    chmod a+r /etc/apt/keyrings/docker.gpg; \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/debian bookworm stable" \
      > /etc/apt/sources.list.d/docker.list; \
    apt-get update; \
    apt-get install -y docker-ce-cli; \
    rm -rf /var/lib/apt/lists/*; \
    docker --version

# Install Bitcoin Core
ARG TARGETARCH
RUN BITCOIN_VERSION=28.1 && \
    if [ "$TARGETARCH" = "amd64" ]; then \
        BITCOIN_ARCH="x86_64-linux-gnu"; \
    elif [ "$TARGETARCH" = "arm64" ]; then \
        BITCOIN_ARCH="aarch64-linux-gnu"; \
    else \
        echo "Unsupported architecture: $TARGETARCH. Supported: amd64, arm64"; \
        exit 1; \
    fi && \
    BITCOIN_FOLDER=bitcoin-${BITCOIN_VERSION} && \
    BITCOIN_URL=https://bitcoincore.org/bin/bitcoin-core-${BITCOIN_VERSION}/bitcoin-${BITCOIN_VERSION}-${BITCOIN_ARCH}.tar.gz && \
    echo "Architecture: $TARGETARCH, downloading Bitcoin Core from: ${BITCOIN_URL}" && \
    cd /tmp && \
    wget ${BITCOIN_URL} && \
    tar -xzf bitcoin-${BITCOIN_VERSION}-${BITCOIN_ARCH}.tar.gz && \
    install -m 0755 -o root -g root -t /usr/local/bin ${BITCOIN_FOLDER}/bin/* && \
    rm -rf /tmp/bitcoin-* && \
    bitcoind --version && \
    bitcoin-cli --version

WORKDIR /workspace

# Install solc-select for versioned solc management.
# Debian enforces PEP 668 ("externally managed environment") for system Python.
# `pipx` is a convenient way to install Python *applications* in an isolated venv.
RUN apt-get update && apt-get install -y \
    python3-full \
    pipx \
    && rm -rf /var/lib/apt/lists/* && \
    pipx install solc-select && \
    ln -sf /root/.local/bin/solc-select /usr/local/bin/solc-select && \
    solc-select --help >/dev/null

# Copy built binaries from bitcoin-ipc builder
COPY --from=bitcoin-ipc-builder /workspace/bitcoin-ipc/target/release/bitcoin-ipc /usr/local/bin/
COPY --from=bitcoin-ipc-builder /workspace/bitcoin-ipc/target/release/monitor /usr/local/bin/
COPY --from=bitcoin-ipc-builder /workspace/bitcoin-ipc/target/release/provider /usr/local/bin/
COPY --from=bitcoin-ipc-builder /workspace/bitcoin-ipc/target/release/quickstart /usr/local/bin/

# Copy IPC binaries
COPY --from=ipc-builder /usr/local/bin/ipc-cli /usr/local/bin/
COPY --from=ipc-builder /usr/local/bin/fendermint /usr/local/bin/

# Copy Foundry binaries
COPY --from=ipc-builder /root/.foundry /root/.foundry

# Copy source code
COPY --from=bitcoin-ipc-builder /workspace/bitcoin-ipc /workspace/bitcoin-ipc
COPY --from=ipc-builder /workspace/ipc /workspace/ipc

# Copy fendermint source for potential runtime build
COPY --from=ipc-builder /workspace/ipc/fendermint /workspace/ipc/fendermint

# Copy cargo-make binary
COPY --from=ipc-builder /usr/local/bin/cargo-make /usr/local/bin/

# Set environment variables
ENV PATH="/usr/local/bin:/root/.foundry/bin:${PATH}"
ENV RUST_LOG=debug
ENV HOME=/root

# Copy entrypoint script from repo so edits don't rebuild builders
COPY /docker-deploy-local/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]

# Default command
CMD ["/bin/bash"]
