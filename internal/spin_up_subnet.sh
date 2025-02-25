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

# Load environment variables from the .env file in the root of the project
ENV_FILE=".env"
if [ ! -f "$ENV_FILE" ]; then
  echo "Error: $ENV_FILE not found!"
  exit 1
fi

# Extract the PROVIDER_AUTH_TOKEN and PROVIDER_PORT values
BEARER_TOKEN=$(grep '^PROVIDER_AUTH_TOKEN=' "$ENV_FILE" | cut -d '=' -f2)
PORT=$(grep '^PROVIDER_PORT=' "$ENV_FILE" | cut -d '=' -f2)

if [ -z "$BEARER_TOKEN" ] || [ -z "$PORT" ]; then
  echo "Error: Required environment variables not found in $ENV_FILE"
  exit 1
fi

API_URL="http://host.docker.internal:${PORT}/api"


echo "Starting validator 1..."
VALIDATOR1_OUTPUT=$(cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
    -e NODE_NAME=validator-1 \
    -e SUBNET_ID="$SUBNET_ID" \
    -e PRIVATE_KEY_PATH="$HOME/.ipc/validator1/validator.sk" \
    -e CMT_P2P_HOST_PORT=26656 \
    -e CMT_RPC_HOST_PORT=26657 \
    -e ETHAPI_HOST_PORT=8545 \
    -e RESOLVER_HOST_PORT=26655 \
    -e PARENT_ENDPOINT="$API_URL" \
    -e PARENT_AUTH_TOKEN="$BEARER_TOKEN" \
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
        -e PARENT_ENDPOINT="$API_URL" \
        -e PARENT_AUTH_TOKEN="$BEARER_TOKEN" \
        -e FM_PULL_SKIP=1 \
        child-validator > /dev/null 2>&1
    echo "Validator $validator_num started!"
}

# Start other validators in parallel
run_validator 2 26756 26757 8645 26755 &
run_validator 3 26856 26857 8745 26855 &
run_validator 4 26956 26957 8845 26955 &

# Wait for all background processes to complete
wait

echo "All validators have been started!"
