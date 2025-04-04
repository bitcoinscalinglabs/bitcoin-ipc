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
BITCOIN_IPC_DIR=$(pwd)
FENDERMINT_MAKEFILE_PATH="../ipc/infra/fendermint/Makefile.toml"

# Create directories for validators if they don't exist
for i in {1..4}; do
    mkdir -p ~/.ipc/validator$i
done

# Prepare environment files for each validator
for i in {1..4}; do
    ENV_FILE=~/.ipc/validator$i/.env
    PORT=$((3029 + $i))

    # Check if the env file already exists
    if [ -f "$ENV_FILE" ]; then
        echo "Using env file for validator $i: $ENV_FILE"
    else
        echo "Creating environment file for validator $i: $ENV_FILE"

        cat > $ENV_FILE << EOL
# Bitcoin Core RPC
RPC_USER=ristic
RPC_PASS=ristic
RPC_URL=http://localhost:18443
WALLET_NAME=validator$i

# Validator
VALIDATOR_SK_PATH=$HOME/.ipc/validator$i/validator.sk

# Provider + Monitor
DATABASE_URL=$HOME/.ipc/validator$i/regtest_db

# Provider
PROVIDER_PORT=$PORT
PROVIDER_AUTH_TOKEN=validator${i}_auth_token

# General
RUST_LOG=bitcoin_ipc=info,monitor=info,provider=info,bitcoincore_rpc=error,actix_web=error
EOL
    fi
done

# Function to mine blocks every 10 seconds
mine_blocks() {
    while true; do
        echo "Mining a new block..."
        bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
        sleep 10
    done
}

# Start monitor and provider for each validator
for i in {1..4}; do
    echo "Starting monitor and provider for validator $i..."
    $BITCOIN_IPC_DIR/target/release/monitor --env ~/.ipc/validator$i/.env &
    sleep 2  # Give monitor time to initialize database
    $BITCOIN_IPC_DIR/target/release/provider --env ~/.ipc/validator$i/.env &
done

echo "All monitors and providers are running. Starting validators..."

echo "Starting validator 1..."
VALIDATOR1_OUTPUT=$(cargo make --makefile "$FENDERMINT_MAKEFILE_PATH" \
    -e NODE_NAME=validator-1 \
    -e SUBNET_ID="$SUBNET_ID" \
    -e PRIVATE_KEY_PATH="$HOME/.ipc/validator1/validator.sk" \
    -e CMT_P2P_HOST_PORT=26656 \
    -e CMT_RPC_HOST_PORT=26657 \
    -e ETHAPI_HOST_PORT=8545 \
    -e RESOLVER_HOST_PORT=26655 \
    -e PARENT_ENDPOINT="http://host.docker.internal:3030/api" \
    -e PARENT_AUTH_TOKEN="validator1_auth_token" \
    -e TOPDOWN_CHAIN_HEAD_DELAY=1 \
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
    local provider_port=$((3029 + $validator_num))

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
        -e PARENT_ENDPOINT="http://host.docker.internal:${provider_port}/api" \
        -e PARENT_AUTH_TOKEN="validator${validator_num}_auth_token" \
        -e TOPDOWN_CHAIN_HEAD_DELAY=1 \
        -e TOPDOWN_PROPOSAL_DELAY=0 \
        -e FM_PULL_SKIP=1 \
        child-validator > /dev/null 2>&1
    echo "Validator $validator_num started!"
}

# Start other validators in parallel
run_validator 2 26756 26757 8645 26755 &
run_validator 3 26856 26857 8745 26855 &
run_validator 4 26956 26957 8845 26955 &

echo "All validators are running. Starting block miner..."
# Start block miner in background
mine_blocks &

echo "System fully operational. Press Ctrl+C to stop all processes."

# Wait for all background processes to complete
wait
