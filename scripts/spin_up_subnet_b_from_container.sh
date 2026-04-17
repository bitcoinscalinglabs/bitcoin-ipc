#!/bin/bash
set -euo pipefail

# Run this INSIDE the bitcoin-ipc container.
#
# Starts fendermint child-validator containers on the HOST (via /var/run/docker.sock),
# pointed at the provider APIs exposed by the bitcoin-ipc container (via host port mappings).
#
# Usage:
#   /workspace/bitcoin-ipc/scripts/spin_up_subnet_b_from_container.sh <subnet_id> [validator5|validator6]
#
# With <subnet_id> only: starts validators 1-4 for Subnet B.
# With <subnet_id> validator5: adds validator 5 (requires CometBftID, ResolverAddress from prior run).
# With <subnet_id> validator6: adds validator 6 (requires CometBftID, ResolverAddress from prior run). Not supported yet.

cleanup() {
    echo -e "\nCleaning up..."
    # Kill all background processes in the same process group
    pkill -P $$ || true
    exit 1
}

trap cleanup SIGINT SIGTERM

if [ "$#" -lt 1 ]; then
    echo "Usage: $0 <subnet_id> [validator5|validator6]" >&2
    echo "  <subnet_id>: required; start validators 1-4 for Subnet B" >&2
    echo "  validator5: optional; add validator 5 (requires CometBftID, ResolverAddress from prior run)" >&2
    echo "  validator6: optional; adds validator 6 (requires CometBftID, ResolverAddress from prior run). Not supported yet." >&2
    exit 1
fi

if [ "$#" -gt 2 ]; then
    echo "Usage: $0 <subnet_id> [validator5|validator6]" >&2
    exit 1
fi

# Reject case: $0 validator5 (user forgot subnet_id; $SUBNET_ID was unset)
if [ "$#" -eq 1 ] && { [ "$1" = "validator5" ] || [ "$1" = "validator6" ]; }; then
    echo "Error: <subnet_id> is required as the first argument." >&2
    echo "Example: $0 \$SUBNET_ID validator5" >&2
    exit 1
fi

SUBNET_ID=$1
if [ -z "$SUBNET_ID" ]; then
    echo "Error: <subnet_id> must be non-empty." >&2
    echo "Example: $0 \$SUBNET_ID" >&2
    exit 1
fi

ADD_VALIDATOR=""
if [ "$#" -eq 2 ]; then
    case "$2" in
        validator5) ADD_VALIDATOR="validator5" ;;
        validator6) ADD_VALIDATOR="validator6" ;;
        *)
            echo "Error: second argument must be validator5 or validator6, got: $2" >&2
            exit 1
            ;;
    esac
fi

if [ -n "$ADD_VALIDATOR" ]; then
    if [ "$ADD_VALIDATOR" = "validator6" ]; then
        echo "The demo does not support validator6 yet" >&2
        exit 1
    fi
    if [ -z "${ResolverAddress:-}" ] || [ -z "${CometBftID:-}" ]; then
        echo "Error: ResolverAddress and CometBftID must be set." >&2
        echo "These are returned by the spin_up_subnet_b_from_container script when we started the containers for Subnet B." >&2
        echo "You need to set these env variables for the following cargo-make command to work." >&2
        exit 1
    fi
fi

if ! command -v docker >/dev/null 2>&1; then
    echo "Error: docker CLI not found in PATH" >&2
    exit 1
fi

if [ ! -S /var/run/docker.sock ]; then
    echo "Error: /var/run/docker.sock is not mounted; cannot start host containers." >&2
    exit 1
fi

if ! command -v cargo-make >/dev/null 2>&1; then
    echo "Error: cargo-make not found in PATH" >&2
    exit 1
fi

# IMPORTANT (local docker + docker.sock):
# The fendermint Makefile uses `docker run --volume ${BASE_DIR}:/data` and commands.
# Because we talk to the host's Docker daemon (via /var/run/docker.sock bind-mount), those paths must
# exist on the host. We therefore set `HOME` for cargo-make to a host-relative path under this repo,
# so BASE_DIR becomes: ${HOST_HOME_DIR}/.ipc/...
if [ -z "${HOST_HOME_DIR:-}" ]; then
    echo "Error: HOST_HOME_DIR is not set (expected from docker-compose environment)." >&2
    exit 1
fi

# IPC repo lives here in the container image.
IPC_DIR="/workspace/ipc"
FENDERMINT_MAKEFILE_PATH="infra/fendermint/Makefile.toml"

if [ ! -d "$IPC_DIR" ]; then
    echo "Error: IPC repo not found at $IPC_DIR" >&2
    exit 1
fi

cd "$IPC_DIR"

# Build fendermint Docker image on-demand if it doesn't exist
if ! docker images --format '{{.Repository}}' | grep -q '^fendermint$'; then
    echo "Building fendermint Docker image (first time only)..."
    cd fendermint && make docker-build
    cd "$IPC_DIR"
fi

# Define hardcoded ports and auth tokens for each validator
PORT_1="3030"
PORT_2="3031"
PORT_3="3032"
PORT_4="3033"
PORT_5="3034"

BEARER_TOKEN_1="validator1_auth_token"
BEARER_TOKEN_2="validator2_auth_token"
BEARER_TOKEN_3="validator3_auth_token"
BEARER_TOKEN_4="validator4_auth_token"
BEARER_TOKEN_5="validator5_auth_token"

API_URL_1="http://host.docker.internal:${PORT_1}/api"
API_URL_2="http://host.docker.internal:${PORT_2}/api"
API_URL_3="http://host.docker.internal:${PORT_3}/api"
API_URL_4="http://host.docker.internal:${PORT_4}/api"
API_URL_5="http://host.docker.internal:${PORT_5}/api"

# Validator 5 fendermint ports (Subnet B: 28056, 28057, 9945, 28055)
CMT_P2P_5="28056"
CMT_RPC_5="28057"
ETHAPI_5="9945"
RESOLVER_5="28055"

if [ "$ADD_VALIDATOR" = "validator5" ]; then
    echo "Starting validator 5 for Subnet B..."
    cargo-make make --makefile "$FENDERMINT_MAKEFILE_PATH" \
        --env NODE_NAME="validator-5-subnet-b" \
        --env SUBNET_ID="$SUBNET_ID" \
        --env HOME="$HOST_HOME_DIR" \
        --env PRIVATE_KEY_PATH="/root/.ipc/validator5/validator.sk" \
        --env CMT_P2P_HOST_PORT="$CMT_P2P_5" \
        --env CMT_RPC_HOST_PORT="$CMT_RPC_5" \
        --env ETHAPI_HOST_PORT="$ETHAPI_5" \
        --env RESOLVER_HOST_PORT="$RESOLVER_5" \
        --env BOOTSTRAPS="${CometBftID}@validator-1-subnet-b-cometbft:26656" \
        --env RESOLVER_BOOTSTRAPS="/dns/validator-1-subnet-b-fendermint/tcp/27655/p2p/${ResolverAddress}" \
        --env PARENT_ENDPOINT="$API_URL_5" \
        --env PARENT_AUTH_TOKEN="$BEARER_TOKEN_5" \
        --env TOPDOWN_CHAIN_HEAD_DELAY=0 \
        --env TOPDOWN_PROPOSAL_DELAY=0 \
        --env FM_PULL_SKIP=1 \
        child-validator
    echo "Validator 5 started!"
    exit 0
fi

echo "Starting validator 1..."

VALIDATOR1_OUTPUT=""
if ! VALIDATOR1_OUTPUT=$(cargo-make make --makefile "$FENDERMINT_MAKEFILE_PATH" \
    --env NODE_NAME="validator-1-subnet-b" \
    --env SUBNET_ID="$SUBNET_ID" \
    --env HOME="$HOST_HOME_DIR" \
    --env PRIVATE_KEY_PATH="/root/.ipc/validator1/validator.sk" \
    --env CMT_P2P_HOST_PORT=27656 \
    --env CMT_RPC_HOST_PORT=27657 \
    --env ETHAPI_HOST_PORT=9545 \
    --env RESOLVER_HOST_PORT=27655 \
    --env PARENT_ENDPOINT="$API_URL_1" \
    --env PARENT_AUTH_TOKEN="$BEARER_TOKEN_1" \
    --env TOPDOWN_CHAIN_HEAD_DELAY=0 \
    --env TOPDOWN_PROPOSAL_DELAY=0 \
    --env FM_PULL_SKIP=1 \
    --env FM_LOG_LEVEL="info,fendermint=info,tower=warn,libp2p=warn,tendermint=warn" \
    child-validator 2>&1); then
    echo "Error: failed to start validator 1" >&2
    echo "$VALIDATOR1_OUTPUT" >&2
    exit 1
fi

# Extract bootstrap information
COMETBFT_ID=$(echo "$VALIDATOR1_OUTPUT" | grep "CometBFT node ID:" -A 1 | tail -n 1 | tr -d ' ' | xargs)
RESOLVER_ADDR=$(echo "$VALIDATOR1_OUTPUT" | grep -o '/ip4/0.0.0.0/tcp/[0-9]*/p2p/[a-zA-Z0-9]*' | cut -d'/' -f7 | xargs)

echo "Bootstrap information:"
echo "CometBFT ID: $COMETBFT_ID"
echo "Resolver Address: $RESOLVER_ADDR"

# Function to run additional validators
run_validator() {
    local validator_num=$1
    local p2p_port=$2
    local rpc_port=$3
    local eth_port=$4
    local resolver_port=$5
    local api_url=$6
    local bearer_token=$7

    echo "Starting validator $validator_num..."

    cargo-make make --makefile "$FENDERMINT_MAKEFILE_PATH" \
        --env NODE_NAME="validator-${validator_num}-subnet-b" \
        --env SUBNET_ID="$SUBNET_ID" \
        --env HOME="$HOST_HOME_DIR" \
        --env PRIVATE_KEY_PATH="/root/.ipc/validator${validator_num}/validator.sk" \
        --env CMT_P2P_HOST_PORT="$p2p_port" \
        --env CMT_RPC_HOST_PORT="$rpc_port" \
        --env ETHAPI_HOST_PORT="$eth_port" \
        --env RESOLVER_HOST_PORT="$resolver_port" \
        --env BOOTSTRAPS="${COMETBFT_ID}@validator-1-subnet-b-cometbft:26656" \
        --env RESOLVER_BOOTSTRAPS="/dns/validator-1-subnet-b-fendermint/tcp/27655/p2p/${RESOLVER_ADDR}" \
        --env PARENT_ENDPOINT="$api_url" \
        --env PARENT_AUTH_TOKEN="$bearer_token" \
        --env TOPDOWN_CHAIN_HEAD_DELAY=0 \
        --env TOPDOWN_PROPOSAL_DELAY=0 \
        --env FM_PULL_SKIP=1 \
        --env FM_LOG_LEVEL="info,fendermint=info,tower=warn,libp2p=warn,tendermint=warn" \
        child-validator > "/root/.ipc/logs/spin-up-subnet-b-validator-${validator_num}.log" 2>&1

    # Verify containers are actually running (cargo-make can exit 0 despite failures)
    for svc in fendermint cometbft ethapi; do
        if ! docker ps --format '{{.Names}}' | grep -q "^validator-${validator_num}-subnet-b-${svc}$"; then
            echo "Error: validator-${validator_num}-subnet-b-${svc} container not running after cargo-make completed." >&2
            return 1
        fi
    done
    echo "Validator $validator_num started!"
}

# Start other validators sequentially (serialized to avoid a macOS Docker Desktop
# shared-volume race in cargo-make's `genesis-write` task: a delayed write flush
# can collide with the subsequent cp, producing "replaced while being copied").
mkdir -p /root/.ipc/logs
for args in \
    "2 27756 27757 9645 27755 $API_URL_2 $BEARER_TOKEN_2" \
    "3 27856 27857 9745 27855 $API_URL_3 $BEARER_TOKEN_3" \
    "4 27956 27957 9845 27955 $API_URL_4 $BEARER_TOKEN_4"
do
    # shellcheck disable=SC2086
    set -- $args
    validator_num=$1
    if ! run_validator "$@"; then
        echo "Error: validator $validator_num failed to start. See log: /root/.ipc/logs/spin-up-subnet-b-validator-${validator_num}.log" >&2
        echo "Aborting." >&2
        exit 1
    fi
done

echo "All validators have been started for subnet $SUBNET_ID!"

# Start relayers for validators 1-4 (logs under /root/.ipc/logs)
mkdir -p /root/.ipc/logs
echo "Starting relayers for validators 1-4..."
for n in 1 2 3 4; do
    RUST_LOG=debug nohup ipc-cli --config-path "/root/.ipc/validator${n}/config.toml" checkpoint relayer --subnet "$SUBNET_ID" >> "/root/.ipc/logs/relayer-subnet-b-validator${n}.log" 2>&1 &
    echo "  Relayer for validator${n} started (log: /root/.ipc/logs/relayer-subnet-b-validator${n}.log)"
done

echo ""
echo "To save bootstrap information, run the following commands:"
echo "export CometBftID=$COMETBFT_ID"
echo "export ResolverAddress=$RESOLVER_ADDR"
