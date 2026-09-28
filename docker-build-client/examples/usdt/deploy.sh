#!/usr/bin/env bash
#
# Deploy BSC's USDT contract (BEP20USDT, unchanged source) on a subnet. The deployer
# becomes the owner and receives the initial supply of 30,000,000 USDT.
#
# Usage:
#   ./deploy.sh --rpc-url https://rpc.testnet.bitcoinscalinglabs.com/<token> \
#     --private-key 0xabcd... --broadcast
#
set -euo pipefail

RPC_URL="${RPC_URL:-}"
PRIVATE_KEY="${PRIVATE_KEY:-}"
BROADCAST=""

usage() {
  echo "usage: $0 --rpc-url <url> --private-key <key> [--broadcast]"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rpc-url)     RPC_URL="$2";            shift 2 ;;
    --private-key) PRIVATE_KEY="$2";        shift 2 ;;
    --broadcast)   BROADCAST="--broadcast"; shift ;;
    *) echo "error: unknown argument '$1'"; usage; exit 1 ;;
  esac
done

[[ -n "$RPC_URL" && -n "$PRIVATE_KEY" ]] || { usage; exit 1; }

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FORGE_CMD=(forge create "src/BEP20USDT.sol:BEP20USDT" --rpc-url "$RPC_URL" --private-key "$PRIVATE_KEY")
[[ -n "$BROADCAST" ]] && FORGE_CMD+=(--broadcast)

DEPLOY_OUTPUT=$(cd "$PROJECT_DIR" && "${FORGE_CMD[@]}" 2>&1) \
  || { echo "error: deployment failed"; echo "$DEPLOY_OUTPUT"; exit 1; }

if [[ -z "$BROADCAST" ]]; then
  echo "Dry run complete (contract compiled). Add --broadcast to deploy."; exit 0
fi

USDT_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep -i "Deployed to:" | awk '{print $NF}')
[[ -n "$USDT_ADDRESS" ]] \
  || { echo "error: could not parse deployed address from forge output:"; echo "$DEPLOY_OUTPUT"; exit 1; }
echo "USDT deployed at: $USDT_ADDRESS"
