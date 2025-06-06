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

# Secret keys for signing (whitelist validator private keys)
SECRET_KEYS=(
  "21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad"
  "67308c2f3915f4c36135f267ed709418c2880025d669e4ada7a206842d53c146"
  "994220215e4601d21a245f8f5e0c407f2f5733ce7907e128c3190c64f4ef443c"
  "ab3a1fafa925836386be55b12fdc92f208ebdad5ef96c0109e4bd06638dcb897"
)

# 1. Generate bootstrap handover PSBT
echo "1. Generating bootstrap handover PSBT..."

HANDOVER_RESPONSE=$(curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"method\": \"genbootstraphandover\",
    \"params\": {
        \"subnet_id\": \"$SUBNET_ID\"
    },
    \"id\": 1
}")

# Check for errors in the response
if echo "$HANDOVER_RESPONSE" | jq -e '.error' > /dev/null; then
  echo "Error generating bootstrap handover PSBT:"
  echo "$HANDOVER_RESPONSE" | jq
  exit 1
fi

# Extract the unsigned PSBT
UNSIGNED_PSBT_BASE64=$(echo "$HANDOVER_RESPONSE" | jq -r '.result.unsigned_psbt_base64')

if [ "$UNSIGNED_PSBT_BASE64" == "null" ] || [ -z "$UNSIGNED_PSBT_BASE64" ]; then
  echo "Error generating bootstrap handover PSBT:"
  echo "$HANDOVER_RESPONSE" | jq
  exit 1
fi

echo "Bootstrap handover PSBT generated successfully"

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

echo "PSBT signed successfully"

# 3. Finalize the bootstrap handover transaction
echo "3. Finalizing bootstrap handover transaction..."

FINALIZE_RESPONSE=$(curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"method\": \"finalizebootstraphandover\",
    \"params\": {
        \"subnet_id\": \"$SUBNET_ID\",
        \"unsigned_psbt_base64\": \"$UNSIGNED_PSBT_BASE64\",
        \"signatures\": $SIGNATURES
    },
    \"id\": 1
}")

# Check for errors in the response
if echo "$FINALIZE_RESPONSE" | jq -e '.error' > /dev/null; then
  echo "Error finalizing bootstrap handover PSBT:"
  echo "$FINALIZE_RESPONSE" | jq
  exit 1
fi

# Extract transaction details
TXID=$(echo "$FINALIZE_RESPONSE" | jq -r '.result.txid')

echo "Bootstrap handover transaction finalized and broadcast successfully"
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
  echo "✅ Bootstrap handover transaction confirmed in block $NEW_BLOCK"
else
  echo "⚠️ Bootstrap handover transaction not found in the new block"
fi

# Get updated subnet state to verify handover was processed
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

echo "Bootstrap handover submission complete!"