#!/bin/bash
set -e

# Build fendermint Docker image if Docker socket is available and image doesn't exist
if command -v docker > /dev/null && docker info > /dev/null 2>&1; then
    if ! docker images | grep -q fendermint; then
        echo "Building fendermint Docker image..."
        cd /workspace/ipc/fendermint
        make docker-build || echo "Warning: fendermint Docker image build failed"
    else
        echo "Fendermint Docker image already exists, I will not build it again."
    fi
else
    echo "Note: Docker not available or socket not mounted. Fendermint image will need to be built separately."
fi

# Initialize Bitcoin config only if bitcoin.conf doesn't exist.
# Do not modify any other contents of /root/.bitcoin.
if [ -f /root/.bitcoin/bitcoin.conf ]; then
    echo "Bitcoin Core config already exists; I will not overwrite it."
else
    echo "Creating Bitcoin Core config..."
    mkdir -p /root/.bitcoin
    cat > /root/.bitcoin/bitcoin.conf <<'EOF'
regtest=1
server=1
rpcuser=user
rpcpassword=pass
rpcallowip=127.0.0.1
fallbackfee=0.00003
paytxfee=0.00003
listen=1
txindex=1
EOF
    echo "Bitcoin Core config created."
fi

# Start Bitcoin Core daemon (with PID file for graceful shutdown)
echo "Starting Bitcoin Core daemon..."
bitcoind -daemon -datadir=/root/.bitcoin -pid=/root/.bitcoin/bitcoind.pid
sleep 3

# Wait for Bitcoin Core to be ready
MAX_RETRIES=3
RETRY_COUNT=0
while [ $RETRY_COUNT -lt $MAX_RETRIES ]; do
    if bitcoin-cli getblockchaininfo > /dev/null 2>&1; then
        echo "Bitcoin Core is ready"
        break
    fi
    echo "Waiting for Bitcoin Core to start... ($RETRY_COUNT/$MAX_RETRIES)"
    sleep 1
    RETRY_COUNT=$((RETRY_COUNT + 1))
done

if [ $RETRY_COUNT -eq $MAX_RETRIES ]; then
    echo "Error: Bitcoin Core failed to start"
    exit 1
fi

# Verify and display Bitcoin Core status
CHAIN=$(bitcoin-cli getblockchaininfo | jq -r '.chain')
echo "Bitcoin Core is running on chain: $CHAIN."

# Run quickstart to generate configuration files
echo "Running quickstart to generate configuration files..."
cd /workspace/bitcoin-ipc
export HOME=/root
if quickstart; then
    echo "Quickstart completed successfully"
else
    EXIT_CODE=$?
    echo "Quickstart completed with exit code $EXIT_CODE"
fi

# Entities we run monitors/providers for
ids="validator1 validator2 validator3 validator4 validator5 validator6 user1 user2"

# Start miner + monitors + providers (background, logs under /root/.ipc/logs)
mkdir -p /root/.ipc/logs
miner_pid=""
monitor_pids=()
monitor_ids=()
provider_pids=()
provider_ids=()

# 0) Start miner
echo "Starting miner..."
bash /workspace/bitcoin-ipc/scripts/miner.sh >> "/root/.ipc/logs/miner.log" 2>&1 &
miner_pid=$!

# Wait 2s, then check miner
sleep 2
if ! kill -0 "$miner_pid" 2>/dev/null; then
    echo "Error: miner did not start. Log:" >&2
    [ -f "/root/.ipc/logs/miner.log" ] && cat "/root/.ipc/logs/miner.log" >&2
fi

# 1) Start all monitors
for id in $ids; do
    echo "Starting monitor for ${id}..."
    monitor --env "/root/.ipc/${id}/.env" >> "/root/.ipc/logs/monitor-${id}.log" 2>&1 &
    monitor_pids+=($!)
    monitor_ids+=("$id")
done

# Wait 2s, then check monitors
sleep 2

i=0
for id in "${monitor_ids[@]}"; do
    if ! kill -0 "${monitor_pids[$i]}" 2>/dev/null; then
        echo "Error: monitor for ${id} did not start. Log:" >&2
        [ -f "/root/.ipc/logs/monitor-${id}.log" ] && cat "/root/.ipc/logs/monitor-${id}.log" >&2
    fi
    i=$((i + 1))
done

# 2) Start all providers
for id in $ids; do
    echo "Starting provider for ${id}..."
    provider --env "/root/.ipc/${id}/.env" >> "/root/.ipc/logs/provider-${id}.log" 2>&1 &
    provider_pids+=($!)
    provider_ids+=("$id")
done

# Wait 2s, then check providers
sleep 2

i=0
for id in "${provider_ids[@]}"; do
    if ! kill -0 "${provider_pids[$i]}" 2>/dev/null; then
        echo "Error: provider for ${id} did not start. Log:" >&2
        [ -f "/root/.ipc/logs/provider-${id}.log" ] && cat "/root/.ipc/logs/provider-${id}.log" >&2
    fi
    i=$((i + 1))
done

# Graceful shutdown: trap SIGTERM/SIGINT and forward to bitcoind, monitor/provider, and main command
shutdown() {
    echo "Shutting down..."
    [ -n "$main_pid" ] && kill -TERM "$main_pid" 2>/dev/null || true
    [ -f /root/.bitcoin/bitcoind.pid ] && kill -TERM $(cat /root/.bitcoin/bitcoind.pid) 2>/dev/null || true
    [ -n "$miner_pid" ] && kill -TERM "$miner_pid" 2>/dev/null || true
    for pid in "${monitor_pids[@]}" "${provider_pids[@]}"; do kill -TERM "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    exit 0
}
trap shutdown SIGTERM SIGINT

# Run main command in foreground (no exec so we remain PID 1 and can trap)
"$@" &
main_pid=$!
wait $main_pid

