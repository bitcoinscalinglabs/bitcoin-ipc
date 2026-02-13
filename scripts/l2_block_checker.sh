#!/bin/bash

# Check if port is provided
if [ -z "$1" ]; then
    echo "Error: Port is required"
    echo "Usage: $0 <port> [checkpoint-period]"
    echo "Example: $0 8545 60"
    exit 1
fi

PORT=$1
CHECKPOINT_PERIOD=${2:-60}

echo "PORT: $PORT"

while true; do
     BLOCK_NUMBER=$(cast block-number --rpc-url http://localhost:$PORT)
     CHECKPOINT_SUFFIX=""

     if [ $((BLOCK_NUMBER % CHECKPOINT_PERIOD)) -eq 0 ]; then
         CHECKPOINT_SUFFIX=" — Checkpoint"
     fi

     echo "Subnet block $BLOCK_NUMBER$CHECKPOINT_SUFFIX"
     sleep 1
done
