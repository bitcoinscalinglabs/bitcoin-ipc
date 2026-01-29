#!/bin/bash
# Helper script to run fendermint validator using cargo make
# This runs cargo make inside the container, so no need to install anything on host
#
# Usage:
#   ./docker-deploy-local/run-fendermint-validator.sh <SUBNET_ID> [validator_number]
#
# Example:
#   ./docker-deploy-local/run-fendermint-validator.sh /b4/t410f... 1

set -e

if [ -z "$1" ]; then
    echo "Usage: $0 <SUBNET_ID> [validator_number]"
    echo "Example: $0 /b4/t410f... 1"
    exit 1
fi

SUBNET_ID=$1
VALIDATOR_NUM=${2:-1}
CONTAINER_NAME=${CONTAINER_NAME:-bitcoin-ipc}

# Calculate ports based on validator number
# validator1: 26656, 26657, 8545, 26655
# validator2: 26756, 26757, 8645, 26755
# etc.
BASE_P2P=$((26656 + (VALIDATOR_NUM - 1) * 1000))
BASE_RPC=$((26657 + (VALIDATOR_NUM - 1) * 1000))
BASE_ETH=$((8545 + (VALIDATOR_NUM - 1) * 100))
BASE_RESOLVER=$((26655 + (VALIDATOR_NUM - 1) * 1000))

# Provider port and auth token
PROVIDER_PORT=$((3030 + VALIDATOR_NUM - 1))
AUTH_TOKEN="validator${VALIDATOR_NUM}_auth_token"

echo "Starting validator ${VALIDATOR_NUM} for subnet ${SUBNET_ID}..."
echo "Using provider at port ${PROVIDER_PORT} with token ${AUTH_TOKEN}"

docker exec -it $CONTAINER_NAME bash -c "
cd /workspace/ipc && \
cargo make --makefile infra/fendermint/Makefile.toml \
    -e NODE_NAME=validator-${VALIDATOR_NUM} \
    -e SUBNET_ID='${SUBNET_ID}' \
    -e PRIVATE_KEY_PATH=/root/.ipc/validator${VALIDATOR_NUM}/validator.sk \
    -e CMT_P2P_HOST_PORT=${BASE_P2P} \
    -e CMT_RPC_HOST_PORT=${BASE_RPC} \
    -e ETHAPI_HOST_PORT=${BASE_ETH} \
    -e RESOLVER_HOST_PORT=${BASE_RESOLVER} \
    -e PARENT_ENDPOINT='http://host.docker.internal:${PROVIDER_PORT}/api' \
    -e PARENT_AUTH_TOKEN='${AUTH_TOKEN}' \
    -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
    -e TOPDOWN_PROPOSAL_DELAY=0 \
    -e FM_PULL_SKIP=1 \
    child-validator
"
