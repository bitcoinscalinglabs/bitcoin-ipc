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

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <subnet_id>"
    exit 1
fi

SUBNET_ID=$1

FENDERMINT_MAKEFILE_PATH="../ipc/infra/fendermint/Makefile.toml"

# Define hardcoded ports and auth tokens for each validator: the subnet B providers, which sign
# the validators' EOTS votes on this subnet
PORT_1="3130"
PORT_2="3131"
PORT_3="3132"
PORT_4="3133"

BEARER_TOKEN_1="validator1_auth_token"
BEARER_TOKEN_2="validator2_auth_token"
BEARER_TOKEN_3="validator3_auth_token"
BEARER_TOKEN_4="validator4_auth_token"

API_URL_1="http://host.docker.internal:${PORT_1}/api"
API_URL_2="http://host.docker.internal:${PORT_2}/api"
API_URL_3="http://host.docker.internal:${PORT_3}/api"
API_URL_4="http://host.docker.internal:${PORT_4}/api"

echo "Starting validator 1..."
VALIDATOR1_OUTPUT=$(cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
    -e NODE_NAME=validator-1-subnet-b \
    -e PERSISTENT_PEERS="" \
    -e SUBNET_ID="$SUBNET_ID" \
    -e PRIVATE_KEY_PATH="$HOME/.ipc/validator1/validator.sk" \
    -e CMT_P2P_HOST_PORT=27656 \
    -e CMT_RPC_HOST_PORT=27657 \
    -e ETHAPI_HOST_PORT=9545 \
    -e RESOLVER_HOST_PORT=27655 \
    -e PARENT_ENDPOINT="$API_URL_1" \
    -e PARENT_AUTH_TOKEN="$BEARER_TOKEN_1" \
    -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
    -e TOPDOWN_PROPOSAL_DELAY=0 \
    -e FM_PULL_SKIP=1 \
    child-validator 2>/dev/null)

# Read a node's ids from the output of its child-validator run
cometbft_id_in() { grep "CometBFT node ID:" -A 1 | tail -n 1 | tr -d ' ' | xargs; }
resolver_id_in() { grep -o '/ip4/0.0.0.0/tcp/[0-9]*/p2p/[a-zA-Z0-9]*' | cut -d'/' -f7 | xargs; }

# Extract bootstrap information
COMETBFT_ID=$(echo "$VALIDATOR1_OUTPUT" | cometbft_id_in)
RESOLVER_ADDR=$(echo "$VALIDATOR1_OUTPUT" | resolver_id_in)

echo "Bootstrap information:"
echo "CometBFT ID: $COMETBFT_ID"
echo "Resolver Address: $RESOLVER_ADDR"

# Peer lists of the next validator to start: all validators started so far. A validator dials its
# CometBFT persistent peers at start and redials them when a connection drops, so every pair
# connects at once. Once all are up, write_full_peer_lists gives each validator the whole list.
COMETBFT_PEERS="${COMETBFT_ID}@validator-1-subnet-b-cometbft:26656"
RESOLVER_PEERS="/dns/validator-1-subnet-b-fendermint/tcp/27655/p2p/${RESOLVER_ADDR}"

# Write the full list of the other validators into the CometBFT config of each validator given.
# A running CometBFT reads it at its next start. Until then every pair stays connected, because
# the later validator of the pair already has the earlier one.
write_full_peer_lists() {
    local n
    for n in "$@"; do
        cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
            -e NODE_NAME="validator-${n}-subnet-b" \
            -e SUBNET_ID="$SUBNET_ID" \
            -e PERSISTENT_PEERS="$(echo "$COMETBFT_PEERS" | tr ',' '\n' | grep -v "@validator-${n}-subnet-b-cometbft:" | paste -sd, -)" \
            set-persistent-peers > /dev/null 2>&1 \
            || { echo "Error: failed to write the peer list of validator $n." >&2; exit 1; }
    done
}

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
    local output
    output=$(cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
        -e NODE_NAME="validator-$validator_num-subnet-b" \
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
        child-validator 2>/dev/null)
    echo "Validator $validator_num started!"

    local cometbft_id resolver_id
    cometbft_id=$(echo "$output" | cometbft_id_in)
    resolver_id=$(echo "$output" | resolver_id_in)
    COMETBFT_PEERS="$COMETBFT_PEERS,${cometbft_id}@validator-${validator_num}-subnet-b-cometbft:26656"
    RESOLVER_PEERS="$RESOLVER_PEERS,/dns/validator-${validator_num}-subnet-b-fendermint/tcp/${resolver_port}/p2p/${resolver_id}"
}

# Start other validators one by one: each needs the ids of those before it
run_validator 2 27756 27757 9645 27755 "$API_URL_2" "$BEARER_TOKEN_2"
run_validator 3 27856 27857 9745 27855 "$API_URL_3" "$BEARER_TOKEN_3"
run_validator 4 27956 27957 9845 27955 "$API_URL_4" "$BEARER_TOKEN_4"

write_full_peer_lists 1 2 3 4

echo "All validators have been started!"

echo ""
echo "To save bootstrap information, run the following commands:"
echo "export CometBftID=$COMETBFT_ID"
echo "export ResolverAddress=$RESOLVER_ADDR"
