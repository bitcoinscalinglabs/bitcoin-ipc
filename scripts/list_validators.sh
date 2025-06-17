#!/bin/bash

# Check if validator number is provided (optional, defaults to 1)
VALIDATOR_NUM=${1:-1}

# Map validator number to RPC port
case $VALIDATOR_NUM in
    1) RPC_PORT=26657 ;;
    2) RPC_PORT=26757 ;;
    3) RPC_PORT=26857 ;;
    4) RPC_PORT=26957 ;;
    5) RPC_PORT=27057 ;;
    *) echo "Error: Invalid validator number. Valid range: 1-5"; exit 1 ;;
esac

echo "Monitoring validators on validator $VALIDATOR_NUM (port $RPC_PORT)..."
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
