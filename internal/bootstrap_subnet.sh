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

# Function to generate a random number between min and max (inclusive)
rand_in_range() {
	local min=$1
	local max=$2
	local raw=$(( min + (RANDOM * RANDOM) % (max - min + 1) ))
	# Round to nearest 10000
	echo $(((raw + 50000) / 100000 * 100000))
}

MIN_VAL=10000000
MAX_VAL=40000000

# 1. Create the subnet and record the subnet ID in a variable
CREATE_OUTPUT=$(curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"createsubnet\",
      \"params\": {
          \"min_validator_stake\": $MIN_VAL,
          \"min_validators\": 4,
          \"bottomup_check_period\": 5,
          \"active_validators_limit\": 4,
          \"min_cross_msg_fee\": 200,
          \"whitelist\": [
              \"5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268\",
              \"851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4\",
              \"b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0\",
              \"b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529\"
          ]
      },
      \"id\": 1
  }")

SUBNET_ID=$(echo "$CREATE_OUTPUT" | jq -r '.result.subnet_id')
echo "Created subnet: $SUBNET_ID"

# 2. Mine a block
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5
BLOCK_NUM=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$BLOCK_NUM")
echo "Subnet created in block number ${BLOCK_NUM} (hash: ${BLOCK_HASH})"

# 3. Join the subnet with 4 validators using random collateral amounts
# Validator 1
COLLATERAL1=$(rand_in_range $MIN_VAL $MAX_VAL)
curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"joinsubnet\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"collateral\": $COLLATERAL1,
          \"ip\": \"66.222.44.55:8080\",
          \"backup_address\": \"bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n\",
          \"pubkey\": \"5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268\"
      },
      \"id\": 1
  }" > /dev/null

# Validator 2
COLLATERAL2=$(rand_in_range $MIN_VAL $MAX_VAL)
curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"joinsubnet\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"collateral\": $COLLATERAL2,
          \"ip\": \"66.222.44.55:8081\",
          \"backup_address\": \"bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n\",
          \"pubkey\": \"851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4\"
      },
      \"id\": 1
  }" > /dev/null

# Validator 3
COLLATERAL3=$(rand_in_range $MIN_VAL $MAX_VAL)
curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"joinsubnet\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"collateral\": $COLLATERAL3,
          \"ip\": \"66.222.44.55:8082\",
          \"backup_address\": \"bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n\",
          \"pubkey\": \"b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0\"
      },
      \"id\": 1
  }" > /dev/null

# Validator 4
COLLATERAL4=$(rand_in_range $MIN_VAL $MAX_VAL)
curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"joinsubnet\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"collateral\": $COLLATERAL4,
          \"ip\": \"66.222.44.55:8083\",
          \"backup_address\": \"bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n\",
          \"pubkey\": \"b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529\"
      },
      \"id\": 1
  }" > /dev/null

# 4. Mine another block
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5
BLOCK_NUM=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$BLOCK_NUM")
echo "Subnet bootstrapped in block number ${BLOCK_NUM} (hash: ${BLOCK_HASH})"

# 5. Fund the subnet with a random amount between 100000000 and 400000000
FUND_AMOUNT=$(rand_in_range $MIN_VAL $MAX_VAL)
curl -s -X POST "$API_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $BEARER_TOKEN" \
  -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"fundsubnet\",
      \"params\": {
          \"subnet_id\": \"$SUBNET_ID\",
          \"amount\": $FUND_AMOUNT,
          \"address\": \"0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf\"
      },
      \"id\": 1
  }" > /dev/null

# 6. Mine one more block
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5
BLOCK_NUM=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$BLOCK_NUM")
echo "Subnet funded in block number ${BLOCK_NUM} (hash: ${BLOCK_HASH})"

# 7. Retrieve and print the subnet state
echo "Subnet state:
"

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
  }" | jq
