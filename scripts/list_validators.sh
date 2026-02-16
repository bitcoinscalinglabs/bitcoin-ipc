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

echo "Monitoring validators on port $RPC_PORT..."
echo "Press Ctrl+C to exit"

clear;
previous_total=""
while true; do
    result=$(curl -s http://localhost:$RPC_PORT/validators | jq '(.result.validators |= (map(del(.proposer_priority)) | sort_by(.address))) | .result')
    current_total=$(echo "$result" | jq -r '.total')

    if [ "$current_total" != "$previous_total" ] && [ -n "$previous_total" ]; then
        clear
    fi

    tput cup 0 0
    echo "$result" | jq
    previous_total="$current_total"
    sleep 1
done
