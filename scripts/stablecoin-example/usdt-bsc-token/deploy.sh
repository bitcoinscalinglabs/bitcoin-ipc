#!/usr/bin/env bash

if [[ "$(pwd)" != *"usdt-bsc-token" ]]; then
  echo "ERROR: Please run this script from within the usdt-bsc-token directory."
  exit 1
fi

if [[ -z "${SUBNET_RPC_URL:-}" ]]; then
  echo "ERROR: SUBNET_RPC_URL is not set."
  echo "Set it as: export SUBNET_RPC_URL=http://localhost:8545"
  exit 1
fi

# Use validator1 (from the demo.ipc deployment) as deployer.
DEPLOYER_PRIVATE_KEY=21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad

forge build .

DEPLOY_OUTPUT=$(forge create src/BEP20USDT.sol:BEP20USDT \
    --rpc-url "$SUBNET_RPC_URL" \
    --private-key "$DEPLOYER_PRIVATE_KEY" \
    # --broadcast
    )

DEPLOYED_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep -i "Deployed to:" | awk '{print $3}')
echo "Deployed contract address: $DEPLOYED_ADDRESS"

ABI_FILE="BEP20USDT.abi"

echo "Generating ABI..."
forge inspect src/BEP20USDT.sol:BEP20USDT abi > $ABI_FILE
echo "ABI saved to $ABI_FILE"