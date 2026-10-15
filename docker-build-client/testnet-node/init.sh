#!/usr/bin/env bash
# One-shot node setup, run by the node-init service inside the fendermint image. Keys and
# genesis are created once; the state-sync trust anchor is refreshed until the node has
# state of its own.
set -euo pipefail
umask 077
: "${NODE_SUBNET_ID:?}" "${SUBNET_ID:?}" "${PROVIDER_AUTH_TOKEN:?}" "${NODE_RPC_SERVERS:?}"

D=/data/validator
K=$D/keys
C=$D/cometbft
PARENT=http://bsl-client:3030/api
FM="fendermint --network=test"

if [ ! -s "$K/validator_key.sk" ]; then
  echo "==> validator and network keys"
  $FM key eth-to-fendermint --secret-key "$K/eth.sk" --name validator_key --out-dir "$K"
  $FM key gen --out-dir "$K" --name network_key
fi
rm -f "$K/eth.sk"
$FM key into-tendermint --secret-key "$K/validator_key.sk" --out "$C/config/priv_validator_key.json"
chmod 600 "$K"/*.sk "$C/config/priv_validator_key.json"

if [ ! -s /data/genesis.committed.json ]; then
  echo "==> waiting for the provider to know $NODE_SUBNET_ID"
  until curl -sf -X POST "$PARENT" -H "Authorization: Bearer $PROVIDER_AUTH_TOKEN" -H 'Content-Type: application/json' \
          -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getgenesisinfo\",\"params\":{\"subnet_id\":\"$NODE_SUBNET_ID\"}}" \
        | grep -q '"bootstrapped":true'; do
    sleep 5
  done

  echo "==> genesis from the parent"
  export EMISSION_CHAIN_FEATURES=$([ "$NODE_SUBNET_ID" = "$SUBNET_ID" ] && echo true || echo false)
  $FM genesis --genesis-file /data/genesis.json ipc from-parent --subnet-id "$NODE_SUBNET_ID" \
      -p "$PARENT" --parent-auth-token "$PROVIDER_AUTH_TOKEN" \
      --parent-gateway 0x56948d2CFaa2EF355B8C08Ac925202db212146D1 \
      --parent-registry 0x6A4884D2B6A597792dC68014D4B7C117cca5668e \
      --base-fee 1000 --power-scale 3 --ipc-btc-emission-subnet "$SUBNET_ID"
  $FM genesis --genesis-file /data/genesis.json set-eam-permissions --mode unrestricted
  $FM genesis --genesis-file /data/genesis.json ipc seal-genesis \
      --builtin-actors-path /fendermint/bundle.car --custom-actors-path /fendermint/custom_actors_bundle.car \
      --artifacts-path /fendermint/contracts --output-path /data/sealed_genesis.car
  $FM genesis --genesis-file /data/genesis.json into-tendermint \
      --app-state /data/sealed_genesis.car --out /data/genesis.committed.json
fi
cp /data/genesis.committed.json "$C/config/genesis.json"

# Trust anchor for state sync: below the newest snapshot (one every 30000 blocks) and well
# inside the trust period. CometBFT ignores it once it has state, so refreshing is always
# safe; a failed refresh only matters while the node has none.
anchor() {
  local rpc tip h hash
  for rpc in ${NODE_RPC_SERVERS//,/ }; do
    tip=$(curl -sf "$rpc/status" | sed -n 's/.*"latest_block_height":"\([0-9]*\)".*/\1/p')
    [ -n "$tip" ] || continue
    h=$((tip - 70000))
    hash=$(curl -sf "$rpc/commit?height=$h" | sed -n 's/.*"block_id":{"hash":"\([A-F0-9]*\)".*/\1/p')
    [ -n "$hash" ] || continue
    sed -i -e "s|^trust_height = .*|trust_height = $h|" -e "s|^trust_hash = .*|trust_hash = \"$hash\"|" "$C/config/config.toml"
    echo "==> state sync anchor: height $h hash $hash"
    return 0
  done
  return 1
}
if ! anchor && [ ! -d "$C/data/state.db" ]; then
  echo "ERROR: no state sync anchor from $NODE_RPC_SERVERS" >&2; exit 1
fi

mkdir -p "$D/fendermint/data" "$D/fendermint/snapshots"
chmod 755 "$D"
chown -R 101:1000 "$C"   # CometBFT runs as its image user
echo "==> node ready"
