#!/usr/bin/env bash
# Open the SSH tunnel to bitcoind, then run monitor + provider against it.
set -euo pipefail

: "${BITCOIN_CONN_MODE:=ssh-tunnel}"
if [ "$BITCOIN_CONN_MODE" != "ssh-tunnel" ]; then
  echo "ERROR: BITCOIN_CONN_MODE='$BITCOIN_CONN_MODE' not supported (only 'ssh-tunnel')." >&2
  exit 1
fi

: "${SSH_HOST:?SSH_HOST required}"
: "${SSH_USER:?SSH_USER required}"
: "${RPC_USER:?RPC_USER required}"
: "${RPC_PASS:?RPC_PASS required}"
: "${WALLET_NAME:?WALLET_NAME required}"
: "${PROVIDER_AUTH_TOKEN:?PROVIDER_AUTH_TOKEN required}"
: "${ACTIVATION_HEIGHT:?ACTIVATION_HEIGHT required}"
: "${SNAPSHOT_LENGTH:?SNAPSHOT_LENGTH required}"
: "${PROVIDER_PORT:=3030}"
: "${DATABASE_URL:=/var/lib/bsl/db}"
: "${MONITOR_SYNC_BATCH_SIZE:=1000}"
: "${SUBNET_ID:=/b4}"
# Optional cross-subnet (fevm) config vars, from bsl.env.
: "${SUBNET2_ID:=}"
: "${URL:=}"
: "${URL2:=}"
: "${TOKEN:=}"
: "${GATEWAY_ADDR:=}"
: "${REGISTRY_ADDR:=}"

: "${RUST_LOG:=error,bitcoin_ipc=info,monitor=info,provider=info}"
TUNNEL_KEY="${TUNNEL_KEY:-/run/tunnel_key}"
LOCAL_RPC_PORT=18443
RPC_URL="http://127.0.0.1:${LOCAL_RPC_PORT}"

[ -f "$TUNNEL_KEY" ] || { echo "ERROR: tunnel key not mounted at $TUNNEL_KEY" >&2; exit 1; }
# ssh rejects a world-readable key, so copy the ro mount to a 600 file.
install -m 600 "$TUNNEL_KEY" /tmp/tunnel_key

echo "==> opening SSH tunnel to ${SSH_USER}@${SSH_HOST}"
autossh -M 0 -f -N \
  -L "${LOCAL_RPC_PORT}:127.0.0.1:18443" \
  -i /tmp/tunnel_key \
  -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
  -o ExitOnForwardFailure=yes -o StrictHostKeyChecking=accept-new \
  "${SSH_USER}@${SSH_HOST}"

echo "==> waiting for bitcoind RPC via tunnel"
for _ in $(seq 1 30); do
  nc -z 127.0.0.1 "$LOCAL_RPC_PORT" && break
  sleep 1
done
nc -z 127.0.0.1 "$LOCAL_RPC_PORT" || { echo "ERROR: tunnel to bitcoind not reachable" >&2; exit 1; }

mkdir -p "$DATABASE_URL" /etc/bsl /root/.ipc
cat > /etc/bsl/stack.env <<EOF
RPC_USER=$RPC_USER
RPC_PASS=$RPC_PASS
RPC_URL=$RPC_URL
WALLET_NAME=$WALLET_NAME
DATABASE_URL=$DATABASE_URL
PROVIDER_PORT=$PROVIDER_PORT
PROVIDER_AUTH_TOKEN=$PROVIDER_AUTH_TOKEN
ACTIVATION_HEIGHT=$ACTIVATION_HEIGHT
SNAPSHOT_LENGTH=$SNAPSHOT_LENGTH
MONITOR_SYNC_BATCH_SIZE=$MONITOR_SYNC_BATCH_SIZE
RUST_LOG=$RUST_LOG
EOF
# No VALIDATOR_SK_PATH: the provider runs read/join-only, which needs no signing key.

# ipc-cli operates on the subnet "as viewed by the parent", so the config entry is
# the parent id (e.g. /b4), not the full child subnet id; commands pass --subnet.
cat > /root/.ipc/config.toml <<EOF
keystore_path = "~/.ipc"

[[subnets]]
id = "${SUBNET_ID%/*}"

[subnets.config]
network_type = "btc"
provider_http = "http://127.0.0.1:${PROVIDER_PORT}/api"
auth_token = "$PROVIDER_AUTH_TOKEN"
EOF

# Populate ipc config file.
append_fevm_subnet() {
  local id="$1" base="$2"
  [ -n "$id" ] && [ -n "$base" ] && [ -n "$GATEWAY_ADDR" ] && [ -n "$REGISTRY_ADDR" ] || return 0
  cat >> /root/.ipc/config.toml <<EOF

[[subnets]]
id = "$id"

[subnets.config]
network_type = "fevm"
provider_http = "${base}${TOKEN}"
gateway_addr = "$GATEWAY_ADDR"
registry_addr = "$REGISTRY_ADDR"
EOF
  echo "  config.toml: added fevm entry for $id"
}
append_fevm_subnet "$SUBNET_ID"  "$URL"
append_fevm_subnet "$SUBNET2_ID" "$URL2"

echo "==> starting monitor"
monitor --env /etc/bsl/stack.env &
MON=$!

# The provider opens the DB read-only and panics if the monitor hasn't created it
# yet. Give the monitor a head start so the first attempt usually wins the race; the
# retry loop below still covers a slower-than-expected monitor start.
sleep 5

echo "==> starting provider"
PRV=""
for _ in $(seq 1 30); do
  provider --env /etc/bsl/stack.env &
  PRV=$!
  sleep 3
  kill -0 "$PRV" 2>/dev/null && break
  kill -0 "$MON" 2>/dev/null || { echo "ERROR: monitor exited during startup" >&2; exit 1; }
  PRV=""
done
[ -n "$PRV" ] && kill -0 "$PRV" 2>/dev/null \
  || { echo "ERROR: provider failed to start" >&2; kill "$MON" 2>/dev/null; exit 1; }

# Exit if either dies so Docker's restart policy restarts the whole container.
wait -n "$MON" "$PRV"
echo "ERROR: monitor or provider exited; shutting down" >&2
kill "$MON" "$PRV" 2>/dev/null || true
exit 1
