#!/usr/bin/env bash
#
# Deploy the BridgeableToken example ERC20 on a subnet and register it for cross-subnet
# bridging (GatewayErcFacet.registerBridgeableToken). forge compiles the contract on first run.
#
# Usage:
#   ./deploy.sh \
#     --rpc-url https://rpc.testnet.bitcoinscalinglabs.com/<token> \
#     --private-key 0xabcd... \
#     --gateway 0x77aa... \
#     --name "MyToken" --symbol "MTK" --decimals 18 \
#     --initial-supply 1000000000000000000000000 \
#     --broadcast
#
set -euo pipefail

NAME=""
SYMBOL=""
DECIMALS=""
INITIAL_SUPPLY=""
BROADCAST=""
RPC_URL="${RPC_URL:-}"
PRIVATE_KEY="${PRIVATE_KEY:-}"
GATEWAY_ADDRESS="${GATEWAY_ADDRESS:-}"

usage() {
  echo "usage: $0 --rpc-url <url> --private-key <key> --gateway <addr> --name <name> --symbol <symbol> --decimals <decimals> --initial-supply <amount> [--broadcast]"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rpc-url)        RPC_URL="$2";            shift 2 ;;
    --private-key)    PRIVATE_KEY="$2";        shift 2 ;;
    --gateway)        GATEWAY_ADDRESS="$2";    shift 2 ;;
    --name)           NAME="$2";               shift 2 ;;
    --symbol)         SYMBOL="$2";             shift 2 ;;
    --decimals)       DECIMALS="$2";           shift 2 ;;
    --initial-supply) INITIAL_SUPPLY="$2";     shift 2 ;;
    --broadcast)      BROADCAST="--broadcast"; shift ;;
    *) echo "error: unknown argument '$1'"; usage; exit 1 ;;
  esac
done

missing=()
[[ -z "$RPC_URL" ]]         && missing+=("--rpc-url")
[[ -z "$PRIVATE_KEY" ]]     && missing+=("--private-key")
[[ -z "$GATEWAY_ADDRESS" ]] && missing+=("--gateway")
[[ -z "$NAME" ]]            && missing+=("--name")
[[ -z "$SYMBOL" ]]          && missing+=("--symbol")
[[ -z "$DECIMALS" ]]        && missing+=("--decimals")
[[ -z "$INITIAL_SUPPLY" ]]  && missing+=("--initial-supply")
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "error: missing required arguments: ${missing[*]}"; usage; exit 1
fi

for tool in forge cast; do
  command -v "$tool" &>/dev/null || { echo "error: '$tool' (Foundry) is not in PATH"; exit 1; }
done

DEPLOYER=$(cast wallet address --private-key "$PRIVATE_KEY" 2>/dev/null) \
  || { echo "error: invalid --private-key"; exit 1; }
echo "Deployer address: $DEPLOYER"

BALANCE=$(cast balance "$DEPLOYER" --rpc-url "$RPC_URL" 2>/dev/null) \
  || { echo "error: cannot connect to --rpc-url $RPC_URL"; exit 1; }
[[ "$BALANCE" == "0" ]] \
  && echo "warning: deployer balance is 0 — transactions will fail without native tokens for gas"

# The project root is this script's directory; forge runs there against the vendored sources.
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo
echo "Deploying BridgeableToken: name=$NAME symbol=$SYMBOL decimals=$DECIMALS initialSupply=$INITIAL_SUPPLY (smallest units) broadcast=${BROADCAST:-<dry-run>}"

FORGE_CMD=(forge create "src/BridgeableToken.sol:BridgeableToken")
[[ -n "$BROADCAST" ]] && FORGE_CMD+=(--broadcast)
FORGE_CMD+=(
  --rpc-url "$RPC_URL"
  --private-key "$PRIVATE_KEY"
  --constructor-args "$NAME" "$SYMBOL" "$DECIMALS" "$INITIAL_SUPPLY" "$DEPLOYER"
)

DEPLOY_OUTPUT=$(cd "$PROJECT_DIR" && "${FORGE_CMD[@]}" 2>&1) \
  || { echo "error: deployment failed"; echo "$DEPLOY_OUTPUT"; exit 1; }

if [[ -z "$BROADCAST" ]]; then
  echo "Dry run complete (contract compiled). Add --broadcast to deploy and register."; exit 0
fi

TOKEN_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep -i "Deployed to:" | awk '{print $NF}')
[[ -n "$TOKEN_ADDRESS" ]] \
  || { echo "error: could not parse deployed address from forge output:"; echo "$DEPLOY_OUTPUT"; exit 1; }
echo "Token deployed at: $TOKEN_ADDRESS"

# Wait for the deploy to confirm so the register tx gets a fresh nonce.
DEPLOY_TX_HASH=$(echo "$DEPLOY_OUTPUT" | grep -i "Transaction hash:" | awk '{print $NF}')
[[ -n "$DEPLOY_TX_HASH" ]] \
  && cast receipt "$DEPLOY_TX_HASH" --rpc-url "$RPC_URL" --confirmations 1 >/dev/null 2>&1 || true

echo
echo "Registering token on gateway $GATEWAY_ADDRESS ..."
REGISTER_OUTPUT=$(
  cast send "$GATEWAY_ADDRESS" "registerBridgeableToken(address)" "$TOKEN_ADDRESS" \
    --rpc-url "$RPC_URL" --private-key "$PRIVATE_KEY" --confirmations 1
) || { echo "error: registration failed"; echo "$REGISTER_OUTPUT"; exit 1; }

echo "Token registered for cross-subnet bridging."
echo
echo "Summary:"
echo "  Token address:  $TOKEN_ADDRESS"
echo "  Name:           $NAME"
echo "  Symbol:         $SYMBOL"
echo "  Decimals:       $DECIMALS"
echo "  Initial supply: $INITIAL_SUPPLY (smallest units)"
echo "  Owner:          $DEPLOYER"
echo "  Gateway:        $GATEWAY_ADDRESS"
