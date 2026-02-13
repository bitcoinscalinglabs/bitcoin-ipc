#!/bin/bash

# Check if RPC port is provided
if [ -z "${1:-}" ]; then
    echo "Error: RPC port of CometBFT node is required"
    echo "Usage: $0 <rpc-port>"
    echo "Example: $0 26657"
    echo "Example: $0 26757"
    echo "Example: $0 27657"
    echo "Example: $0 27757"
    exit 1
fi

RPC_PORT="$1"

echo "Monitoring validator status on port $RPC_PORT..."
echo "Press Ctrl+C to exit"

while true; do
    clear
    curl -s "http://localhost:$RPC_PORT/status" | jq
    sleep 1
done
