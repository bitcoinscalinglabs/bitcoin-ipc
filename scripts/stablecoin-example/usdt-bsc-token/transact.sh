# Basic commands to test interaction with the deployed token.

if [[ -z "${SUBNET_RPC_URL:-}" ]]; then
  echo "ERROR: SUBNET_RPC_URL is not set."
  echo "Set it as: export SUBNET_RPC_URL=http://localhost:8545"
  exit 1
fi

if [[ -z "${TOKEN_ADDRESS:-}" ]]; then
  echo "ERROR: TOKEN_ADDRESS is not set."
  echo "Set it as: export TOKEN_ADDRESS=<deployed_contract_address>"
  exit 1
fi

TO="0xb8c4486622484150084a8E1Ee6687e17fEBE6229"
DEPLOYER_PRIVATE_KEY="21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad"
DEPLOYER_ADDRESS="0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8"
# cast wallet address --private-key "$DEPLOYER_PRIVATE_KEY"

# Check decimals
cast call --rpc-url "$SUBNET_RPC_URL" "$TOKEN_ADDRESS" "decimals()(uint8)"

# Mint 100 tokens (assuming 18 decimals):
cast send --rpc-url "$SUBNET_RPC_URL" --private-key "$DEPLOYER_PRIVATE_KEY" \
  "$TOKEN_ADDRESS" "mint(uint256)" "$(cast --to-wei 100 ether)"

# Check balance of the deployer
cast call --rpc-url "$SUBNET_RPC_URL" "$TOKEN_ADDRESS" "balanceOf(address)(uint256)" "$DEPLOYER_ADDRESS"

# Transfer 5 tokens to the recipient:
cast send --rpc-url "$SUBNET_RPC_URL" --private-key "$DEPLOYER_PRIVATE_KEY" \
  "$TOKEN_ADDRESS" "transfer(address,uint256)" "$TO" "$(cast --to-wei 5 ether)"

# Check balance of the recipient:
cast call --rpc-url "$SUBNET_RPC_URL" "$TOKEN_ADDRESS" "balanceOf(address)(uint256)" "$TO"