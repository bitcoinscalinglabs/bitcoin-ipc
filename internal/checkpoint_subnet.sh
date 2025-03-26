
#!/bin/bash
set -e

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

API_URL="http://localhost:${PORT}/api"

# Check if a subnet ID is provided as an argument
if [ -z "$1" ]; then
  echo "Usage: $0 <subnet_id>"
  exit 1
fi

SUBNET_ID="$1"
echo "Working with subnet: $SUBNET_ID"

# Secret keys for signing (validator private keys)
SECRET_KEYS=(
  "21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad"
  "67308c2f3915f4c36135f267ed709418c2880025d669e4ada7a206842d53c146"
  "994220215e4601d21a245f8f5e0c407f2f5733ce7907e128c3190c64f4ef443c"
  "ab3a1fafa925836386be55b12fdc92f208ebdad5ef96c0109e4bd06638dcb897"
)

# Generate a random checkpoint hash for testing
RANDOM_HASH=$(openssl rand -hex 32)

# Get the current block number
CURRENT_BLOCK=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$CURRENT_BLOCK")

DESTINATION_SUBNET_ID_1="/b4/t420fxivdpexcejskneatarvuth2qv7ncn5mbod7x4w4lm5h575rzq6depsiklm"

echo "Current block: $CURRENT_BLOCK ($BLOCK_HASH)"
echo "Using checkpoint hash: $RANDOM_HASH"

CHECKPOINT_RESPONSE=$(curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"method\": \"gencheckpointpsbt\",
    \"params\": {
        \"subnet_id\": \"$SUBNET_ID\",
        \"checkpoint_hash\": \"$RANDOM_HASH\",
        \"checkpoint_height\": 50,
        \"withdrawals\": [
            {
                \"amount\": 25000,
                \"address\": \"bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n\"
            }
        ],
        \"transfers\": [
            {
                \"amount\": 150000,
                \"destination_subnet_id\": \"$DESTINATION_SUBNET_ID_1\",
                \"subnet_user_address\": \"0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf\"
            },
            {
                \"amount\": 100000,
                \"destination_subnet_id\": \"$DESTINATION_SUBNET_ID_1\",
                \"subnet_user_address\": \"0x4967bB72907683bb6a933d47348a49bC3832968b\"
            }
        ]
    },
    \"id\": 1
}")

# Extract the unsigned PSBT
UNSIGNED_PSBT_BASE64=$(echo "$CHECKPOINT_RESPONSE" | jq -r '.result.unsigned_psbt_base64')
BATCH_TRANSFER_TX_HEX=$(echo "$CHECKPOINT_RESPONSE" | jq -r '.result.batch_transfer_tx_hex')

if [ "$UNSIGNED_PSBT_BASE64" == "null" ] || [ -z "$UNSIGNED_PSBT_BASE64" ]; then
  echo "Error generating checkpoint PSBT:"
  echo "$CHECKPOINT_RESPONSE" | jq
  exit 1
fi

# 2. Sign the PSBT using dev_multisignpsbt
echo "2. Signing PSBT with development keys..."

# Construct the JSON array of secret keys
SECRET_KEYS_JSON=$(printf '%s\n' "${SECRET_KEYS[@]}" | jq -R . | jq -s .)

SIGN_RESPONSE=$(curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"method\": \"dev_multisignpsbt\",
    \"params\": {
        \"unsigned_psbt_base64\": \"$UNSIGNED_PSBT_BASE64\",
        \"secret_keys\": $SECRET_KEYS_JSON
    },
    \"id\": 1
}")

# Check for errors in the response
if echo "$SIGN_RESPONSE" | jq -e '.error' > /dev/null; then
  echo "Error signing PSBT:"
  echo "$SIGN_RESPONSE" | jq
  exit 1
fi

# Extract the signatures
SIGNATURES=$(echo "$SIGN_RESPONSE" | jq '.result.signatures')

if [ "$SIGNATURES" == "null" ] || [ -z "$SIGNATURES" ]; then
  echo "Error: No signatures returned"
  echo "$SIGN_RESPONSE" | jq
  exit 1
fi

# 3. Finalize the checkpoint transaction
echo "3. Finalizing checkpoint transaction..."

if [ "$BATCH_TRANSFER_TX_HEX" != "null" ] && [ -n "$BATCH_TRANSFER_TX_HEX" ]; then
  FINALIZE_RESPONSE=$(curl -s -X POST "$API_URL" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $BEARER_TOKEN" \
    -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"finalizecheckpointpsbt\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"unsigned_psbt_base64\": \"$UNSIGNED_PSBT_BASE64\",
          \"signatures\": $SIGNATURES,
          \"batch_transfer_tx_hex\": \"$BATCH_TRANSFER_TX_HEX\"
      },
      \"id\": 1
  }")
else
  FINALIZE_RESPONSE=$(curl -s -X POST "$API_URL" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $BEARER_TOKEN" \
    -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"finalizecheckpointpsbt\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"unsigned_psbt_base64\": \"$UNSIGNED_PSBT_BASE64\",
          \"signatures\": $SIGNATURES
      },
      \"id\": 1
  }")
fi

# Check for errors in the response
if echo "$FINALIZE_RESPONSE" | jq -e '.error' > /dev/null; then
  echo "Error finalizing PSBT:"
  echo "$FINALIZE_RESPONSE" | jq
  exit 1
fi

# Extract transaction details
TXID=$(echo "$FINALIZE_RESPONSE" | jq -r '.result.txid')

echo "Checkpoint transaction finalized and broadcast successfully"
echo "Transaction ID: $TXID"

sleep 1

# 4. Mine a block to confirm the transaction
echo "4. Mining a block to confirm the transaction..."
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5

# Check that our transaction was included in the block
NEW_BLOCK=$(bitcoin-cli getblockcount)
NEW_BLOCK_HASH=$(bitcoin-cli getblockhash "$NEW_BLOCK")
BLOCK_INFO=$(bitcoin-cli getblock "$NEW_BLOCK_HASH")

echo "New block mined: $NEW_BLOCK ($NEW_BLOCK_HASH)"

if [[ $BLOCK_INFO == *"$TXID"* ]]; then
  echo "✅ Checkpoint transaction confirmed in block $NEW_BLOCK"
else
  echo "⚠️ Checkpoint transaction not found in the new block"
fi

# Get updated subnet state to verify checkpoint was processed
echo "Getting updated subnet state..."
curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"method\": \"getsubnet\",
    \"params\": {
        \"subnet_id\": \"$SUBNET_ID\"
    },
    \"id\": 1
}" | jq '.result'

if [ "$BATCH_TRANSFER_TX_HEX" != "null" ] && [ -n "$BATCH_TRANSFER_TX_HEX" ]; then
	echo "Getting rootnet messages state..."
	curl -s -X POST "$API_URL" \
	  -H "Content-Type: application/json" \
	  -H "Authorization: Bearer $BEARER_TOKEN" \
	  -d "{
	      \"jsonrpc\": \"2.0\",
	      \"method\": \"getrootnetmessages\",
	      \"params\": {
	          \"subnet_id\": \"$DESTINATION_SUBNET_ID_1\",
		      \"block_height\": $NEW_BLOCK
	      },
	      \"id\": 1
	  }" | jq
fi

echo "Checkpoint submission complete!"
