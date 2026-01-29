#!/bin/bash
set -e

# Bootstrap a BTC subnet locally (run inside the container):
# - Creates a subnet (via validator1 provider), mines blocks, has validators 1-4 join, funds it, mines more, and prints subnet state.

API_HOST="127.0.0.1"

VAL1_PORT=3030
VAL2_PORT=3031
VAL3_PORT=3032
VAL4_PORT=3033

VAL1_BEARER_TOKEN="validator1_auth_token"
VAL2_BEARER_TOKEN="validator2_auth_token"
VAL3_BEARER_TOKEN="validator3_auth_token"
VAL4_BEARER_TOKEN="validator4_auth_token"

VAL1_API_URL="http://${API_HOST}:${VAL1_PORT}/api"
VAL2_API_URL="http://${API_HOST}:${VAL2_PORT}/api"
VAL3_API_URL="http://${API_HOST}:${VAL3_PORT}/api"
VAL4_API_URL="http://${API_HOST}:${VAL4_PORT}/api"

# Fixed collateral amounts
COLLATERAL1=200000000
COLLATERAL2=110000000
COLLATERAL3=150000000
COLLATERAL4=180000000

# Fixed fund amounts
FUND1=210000000
FUND2=220000000
FUND3=230000000
FUND4=240000000

rpc_post_raw_or_die() {
  local label="$1"
  local api_url="$2"
  local bearer="$3"
  local payload="$4"

  local resp=""
  if ! resp=$(curl -sS --fail-with-body -X POST "$api_url" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $bearer" \
    -d "$payload"); then
    echo "Error: ${label} request failed" >&2
    if [ -n "$resp" ]; then
      echo "$resp" >&2
    fi
    exit 1
  fi

  # Successful HTTP response. Try to parse JSON to detect JSON-RPC errors.
  if ! echo "$resp" | jq -e . >/dev/null 2>&1; then
    echo "Error: ${label} returned non-JSON/invalid JSON response" >&2
    echo "$resp" >&2
    exit 1
  else
    if echo "$resp" | jq -e '.error != null' >/dev/null 2>&1; then
      echo "Error: ${label} returned JSON-RPC error" >&2
      echo "$resp" | jq >&2
      exit 1
    fi
  fi

  printf '%s' "$resp"
}

# 1. Create the subnet and record the subnet ID in a variable
echo "Calling createsubnet..."
CREATE_PAYLOAD=$(cat <<'JSON'
{
  "jsonrpc": "2.0",
  "method": "createsubnet",
  "params": {
    "min_validator_stake": 100000000,
    "min_validators": 4,
    "bottomup_check_period": 60,
    "active_validators_limit": 10,
    "min_cross_msg_fee": 10,
    "whitelist": [
      "5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268",
      "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0",
      "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4",
      "b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529"
    ]
  },
  "id": 1
}
JSON
)

# echo "curl -s -X POST \"$VAL1_API_URL\" \\"
# echo "  -H \"Content-Type: application/json\" \\"
# echo "  -H \"Authorization: Bearer $VAL1_BEARER_TOKEN\" \\"
# echo "  -d '$CREATE_PAYLOAD'"

CREATE_OUTPUT=$(rpc_post_raw_or_die "createsubnet" "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$CREATE_PAYLOAD")

# echo "$CREATE_OUTPUT" | jq

SUBNET_ID=$(echo "$CREATE_OUTPUT" | jq -r '.result.subnet_id // empty')
if [ -z "$SUBNET_ID" ] || [ "$SUBNET_ID" = "null" ]; then
  echo "Error: createsubnet returned empty subnet_id" >&2
  echo "$CREATE_OUTPUT" | jq >&2 || echo "$CREATE_OUTPUT" >&2
  exit 1
fi
echo "Created subnet: $SUBNET_ID"

# 2. Mine a block
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5
BLOCK_NUM=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$BLOCK_NUM")
echo "Subnet created in block number ${BLOCK_NUM} (hash: ${BLOCK_HASH})"

# Record creation metadata for later (we add config entries at the end)
SUBNET_CREATED_AT=$(date -u +"%Y-%m-%d %H:%M:%SZ")
SUBNET_CREATED_HEIGHT="$BLOCK_NUM"

# 3. Join the subnet with 4 validators using fixed collateral amounts and per-validator bearer token/port

# Validator 1
echo "Calling joinsubnet for validator1..."
JOIN1_PAYLOAD=$(cat <<JSON
{
  "jsonrpc": "2.0",
  "method": "joinsubnet",
  "params": {
    "subnet_id": "$SUBNET_ID",
    "collateral": $COLLATERAL1,
    "ip": "66.222.44.55:8080",
    "backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
    "pubkey": "5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268"
  },
  "id": 1
}
JSON
)

# echo "curl -s -X POST \"$VAL1_API_URL\" \\"
# echo "  -H \"Content-Type: application/json\" \\"
# echo "  -H \"Authorization: Bearer $VAL1_BEARER_TOKEN\" \\"
# echo "  -d '$JOIN1_PAYLOAD'"

rpc_post_raw_or_die "joinsubnet (validator1)" "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$JOIN1_PAYLOAD" | jq

# Validator 2
echo "Calling joinsubnet for validator2..."
JOIN2_PAYLOAD=$(cat <<JSON
{
  "jsonrpc": "2.0",
  "method": "joinsubnet",
  "params": {
    "subnet_id": "$SUBNET_ID",
    "collateral": $COLLATERAL2,
    "ip": "66.222.44.55:8081",
    "backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
    "pubkey": "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4"
  },
  "id": 1
}
JSON
)
rpc_post_raw_or_die "joinsubnet (validator2)" "$VAL2_API_URL" "$VAL2_BEARER_TOKEN" "$JOIN2_PAYLOAD" | jq

# Validator 3
echo "Calling joinsubnet for validator3..."
JOIN3_PAYLOAD=$(cat <<JSON
{
  "jsonrpc": "2.0",
  "method": "joinsubnet",
  "params": {
    "subnet_id": "$SUBNET_ID",
    "collateral": $COLLATERAL3,
    "ip": "66.222.44.55:8082",
    "backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
    "pubkey": "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0"
  },
  "id": 1
}
JSON
)
rpc_post_raw_or_die "joinsubnet (validator3)" "$VAL3_API_URL" "$VAL3_BEARER_TOKEN" "$JOIN3_PAYLOAD" | jq

# Validator 4
echo "Calling joinsubnet for validator4..."
JOIN4_PAYLOAD=$(cat <<JSON
{
  "jsonrpc": "2.0",
  "method": "joinsubnet",
  "params": {
    "subnet_id": "$SUBNET_ID",
    "collateral": $COLLATERAL4,
    "ip": "66.222.44.55:8083",
    "backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
    "pubkey": "b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529"
  },
  "id": 1
}
JSON
)
rpc_post_raw_or_die "joinsubnet (validator4)" "$VAL4_API_URL" "$VAL4_BEARER_TOKEN" "$JOIN4_PAYLOAD" | jq

# 4. Mine another block
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5
BLOCK_NUM=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$BLOCK_NUM")
echo "Subnet bootstrapped in block number ${BLOCK_NUM} (hash: ${BLOCK_HASH})"

fund_subnet() {
    local api_url=$1
    local bearer=$2
    local subnet_id=$3
    local eth_address=$4
    local amount=$5

    local payload
    payload=$(cat <<JSON
{
  "jsonrpc": "2.0",
  "method": "fundsubnet",
  "params": {
    "subnet_id": "$subnet_id",
    "amount": $amount,
    "address": "$eth_address"
  },
  "id": 1
}
JSON
)
    rpc_post_raw_or_die "fundsubnet (${eth_address})" "$api_url" "$bearer" "$payload" > /dev/null

    echo "Funded subnet with $amount satoshis to address $eth_address"
}

# 5. Fund the subnet with fixed amounts (use validator1 provider)
echo "Calling fund_subnet for validator1..."
fund_subnet "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$SUBNET_ID" "0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8" "$FUND1"
echo "Calling fund_subnet for validator2..."
fund_subnet "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$SUBNET_ID" "0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414" "$FUND2"
echo "Calling fund_subnet for validator3..."
fund_subnet "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$SUBNET_ID" "0x646Aed5404567ae15648E9b9B0004cbAfb126949" "$FUND3"
echo "Calling fund_subnet for validator4..."
fund_subnet "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$SUBNET_ID" "0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf" "$FUND4"

# 6. Mine one more blocks
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
sleep 3.5
BLOCK_NUM=$(bitcoin-cli getblockcount)
BLOCK_HASH=$(bitcoin-cli getblockhash "$BLOCK_NUM")
echo "Subnet funded in block number ${BLOCK_NUM} (hash: ${BLOCK_HASH})"

bitcoin-cli generatetoaddress 2 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null

# 7. Retrieve and print the subnet state (use validator1 provider)
echo "Subnet state:"
echo "Calling getsubnet..."
GETSUBNET_PAYLOAD=$(cat <<JSON
{
  "jsonrpc": "2.0",
  "method": "getsubnet",
  "params": {
    "subnet_id": "$SUBNET_ID"
  },
  "id": 1
}
JSON
)
rpc_post_raw_or_die "getsubnet" "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" "$GETSUBNET_PAYLOAD" | jq

# Add subnet configuration entry to all IPC config files (end of script)
CONFIG_FILES=(
  /root/.ipc/config.toml
  /root/.ipc/validator1/config.toml
  /root/.ipc/validator2/config.toml
  /root/.ipc/validator3/config.toml
  /root/.ipc/validator4/config.toml
  /root/.ipc/validator5/config.toml
  /root/.ipc/user1/config.toml
  /root/.ipc/user2/config.toml
)

echo "Adding subnet configuration to:"
for f in "${CONFIG_FILES[@]}"; do
  echo "  - $f"
done

for f in "${CONFIG_FILES[@]}"; do
  if [ ! -f "$f" ]; then
    echo "Error: config file not found: $f" >&2
    exit 1
  fi
  cat >> "$f" <<EOF

# Subnet : ${SUBNET_ID}, created on ${SUBNET_CREATED_AT} at height ${SUBNET_CREATED_HEIGHT}
[[subnets]]
id = "${SUBNET_ID}"

[subnets.config]
network_type = "fevm"
provider_http = "http://host.docker.internal:8545/"
gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
EOF
done
echo "Configuration added successfully."

# Print an export command so you can easily set SUBNET_ID in your current shell.
echo "Run the following to export SUBNET_ID in your current shell:"
echo "export SUBNET_ID=\"$SUBNET_ID\""