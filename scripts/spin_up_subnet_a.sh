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
VALIDATOR1_OUTPUT=$(cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
    -e NODE_NAME=validator-1 \
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
    child-validator 2>/dev/null)

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
    cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
        -e NODE_NAME="validator-$validator_num" \
        -e SUBNET_ID="$SUBNET_ID" \
        -e PRIVATE_KEY_PATH="$HOME/.ipc/validator${validator_num}/validator.sk" \
        -e CMT_P2P_HOST_PORT="$p2p_port" \
        -e CMT_RPC_HOST_PORT="$rpc_port" \
        -e ETHAPI_HOST_PORT="$eth_port" \
        -e RESOLVER_HOST_PORT="$resolver_port" \
        -e BOOTSTRAPS="${COMETBFT_ID}@validator-1-cometbft:26656" \
        -e RESOLVER_BOOTSTRAPS="/dns/validator-1-fendermint/tcp/26655/p2p/${RESOLVER_ADDR}" \
        -e PARENT_ENDPOINT="$api_url" \
        -e PARENT_AUTH_TOKEN="$bearer_token" \
        -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
        -e TOPDOWN_PROPOSAL_DELAY=0 \
        -e FM_PULL_SKIP=1 \
        child-validator > /dev/null 2>&1
    echo "Validator $validator_num started!"
}

# Start other validators in parallel
run_validator 2 26756 26757 8645 26755 "$API_URL_2" "$BEARER_TOKEN_2" &
run_validator 3 26856 26857 8745 26855 "$API_URL_3" "$BEARER_TOKEN_3" &
run_validator 4 26956 26957 8845 26955 "$API_URL_4" "$BEARER_TOKEN_4" &

# Wait for all background processes to complete
wait

echo "All validators have been started!"

echo ""
echo "To save bootstrap information, run the following commands:"
echo "export CometBftID=$COMETBFT_ID"
echo "export ResolverAddress=$RESOLVER_ADDR"
