FROM rust:1.87.0-slim AS build-bitcoinipc
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl pkg-config libssl-dev build-essential ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY bitcoin-ipc/ .
RUN cargo build --release --features emission_chain --bin monitor --bin provider

FROM rust:bookworm AS build-ipc
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential libssl-dev pkg-config curl clang git jq ca-certificates protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY ipc/ .
RUN cargo build --release -p ipc-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 autossh openssh-client curl jq netcat-openbsd \
 && rm -rf /var/lib/apt/lists/*
# TARGETARCH (amd64|arm64) is supplied by buildx and matches Foundry's naming.
ARG TARGETARCH
RUN curl -fsSL "https://github.com/foundry-rs/foundry/releases/download/stable/foundry_stable_linux_${TARGETARCH}.tar.gz" \
    | tar -xz -C /usr/local/bin cast forge
COPY --from=build-bitcoinipc /src/target/release/monitor  /usr/local/bin/monitor
COPY --from=build-bitcoinipc /src/target/release/provider /usr/local/bin/provider
COPY --from=build-ipc        /src/target/release/ipc-cli  /usr/local/bin/ipc-cli
# Example ERC-20 for cross-subnet transfers; deploy.sh builds and deploys it.
WORKDIR /opt/bsl/examples/bridgeable-token
COPY bitcoin-ipc/docker-build-client/examples/bridgeable-token/ .
COPY ipc/contracts/contracts/examples/BridgeableToken.sol src/BridgeableToken.sol
RUN curl -fsSL https://github.com/OpenZeppelin/openzeppelin-contracts/archive/refs/tags/v5.0.2.tar.gz \
      | tar -xz && mkdir -p lib && mv openzeppelin-contracts-5.0.2 lib/openzeppelin-contracts \
 && chmod +x deploy.sh
WORKDIR /
COPY bitcoin-ipc/docker-build-client/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
