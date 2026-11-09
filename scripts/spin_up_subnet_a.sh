#!/bin/bash
set -e

cleanup() {
    echo -e "\nCleaning up..."
    # Kill all background processes in the same process group
    pkill -P $$ || true
    exit 1
}

# Set up trap for Ctrl+C (SIGINT) and SIGTERM
trap cleanup SIGINT SIGTERM

source "$(dirname "${BASH_SOURCE[0]}")/spin_up_peers.sh"

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <subnet_id>"
    exit 1
fi

SUBNET_ID=$1

FENDERMINT_MAKEFILE_PATH="../ipc/infra/fendermint/Makefile.toml"
NODE_NAME_SUFFIX=""
CARGO_MAKE=(cargo make --makefile "$FENDERMINT_MAKEFILE_PATH")

# Define hardcoded ports and auth tokens for each validator
PORT_1="3030"
PORT_2="3031"
PORT_3="3032"
PORT_4="3033"

BEARER_TOKEN_1="validator1_auth_token"
BEARER_TOKEN_2="validator2_auth_token"
BEARER_TOKEN_3="validator3_auth_token"
BEARER_TOKEN_4="validator4_auth_token"

API_URL_1="http://host.docker.internal:${PORT_1}/api"
API_URL_2="http://host.docker.internal:${PORT_2}/api"
API_URL_3="http://host.docker.internal:${PORT_3}/api"
API_URL_4="http://host.docker.internal:${PORT_4}/api"

echo "Starting validator 1..."
cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
    -e NODE_NAME=validator-1 \
    -e PERSISTENT_PEERS="" \
    -e SUBNET_ID="$SUBNET_ID" \
    -e PRIVATE_KEY_PATH="$HOME/.ipc/validator1/validator.sk" \
    -e CMT_P2P_HOST_PORT=26656 \
    -e CMT_RPC_HOST_PORT=26657 \
    -e ETHAPI_HOST_PORT=8545 \
    -e RESOLVER_HOST_PORT=26655 \
    -e PARENT_ENDPOINT="$API_URL_1" \
    -e PARENT_AUTH_TOKEN="$BEARER_TOKEN_1" \
    -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
    -e TOPDOWN_PROPOSAL_DELAY=0 \
    -e FM_PULL_SKIP=1 \
    child-validator > /dev/null 2>&1

# Extract bootstrap information
COMETBFT_ID=$(cometbft_node_id 1)
RESOLVER_ADDR=$(resolver_node_id 1)

echo "Bootstrap information:"
echo "CometBFT ID: $COMETBFT_ID"
echo "Resolver Address: $RESOLVER_ADDR"

add_peer 1 26655 "$COMETBFT_ID" "$RESOLVER_ADDR"

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
    cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
        -e NODE_NAME="validator-$validator_num" \
        -e SUBNET_ID="$SUBNET_ID" \
        -e PRIVATE_KEY_PATH="$HOME/.ipc/validator${validator_num}/validator.sk" \
        -e CMT_P2P_HOST_PORT="$p2p_port" \
        -e CMT_RPC_HOST_PORT="$rpc_port" \
        -e ETHAPI_HOST_PORT="$eth_port" \
        -e RESOLVER_HOST_PORT="$resolver_port" \
        -e PERSISTENT_PEERS="$COMETBFT_PEERS" \
        -e RESOLVER_BOOTSTRAPS="$RESOLVER_PEERS" \
        -e PARENT_ENDPOINT="$api_url" \
        -e PARENT_AUTH_TOKEN="$bearer_token" \
        -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
        -e TOPDOWN_PROPOSAL_DELAY=0 \
        -e FM_PULL_SKIP=1 \
        child-validator > /dev/null 2>&1
    echo "Validator $validator_num started!"

    add_node_peer "$validator_num" "$resolver_port"
}

# Start other validators one by one: each needs the ids of those before it
run_validator 2 26756 26757 8645 26755 "$API_URL_2" "$BEARER_TOKEN_2"
run_validator 3 26856 26857 8745 26855 "$API_URL_3" "$BEARER_TOKEN_3"
run_validator 4 26956 26957 8845 26955 "$API_URL_4" "$BEARER_TOKEN_4"

write_full_peer_lists 1 2 3 4

echo "All validators have been started!"

echo ""
echo "To save bootstrap information, run the following commands:"
echo "export CometBftID=$COMETBFT_ID"
echo "export ResolverAddress=$RESOLVER_ADDR"
