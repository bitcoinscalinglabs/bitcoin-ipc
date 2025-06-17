#!/bin/bash

# Check if validator number is provided
if [ -z "$1" ]; then
    echo "Error: Validator number is required"
    echo "Usage: $0 <validator-number>"
    echo "Example: $0 1"
    echo "Example: $0 5"
    exit 1
fi

VALIDATOR_NUM=$1

# Map validator number to RPC port
case $VALIDATOR_NUM in
    1) RPC_PORT=26657 ;;
    2) RPC_PORT=26757 ;;
    3) RPC_PORT=26857 ;;
    4) RPC_PORT=26957 ;;
    5) RPC_PORT=27057 ;;
    *) echo "Error: Invalid validator number. Valid range: 1-5"; exit 1 ;;
esac

echo "Monitoring validator $VALIDATOR_NUM status on port $RPC_PORT..."
echo "Press Ctrl+C to exit"

while true; do
    clear
    curl -s "http://localhost:$RPC_PORT/status" | jq
    sleep 1
done
