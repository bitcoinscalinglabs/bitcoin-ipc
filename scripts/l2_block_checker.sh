#!/bin/bash

# Check if subnet ID is provided
if [ -z "$1" ]; then
    echo "Error: Subnet ID is required"
    echo "Usage: $0 <subnet-id> [checkpoint-period]"
    echo "Example: $0 /b4/t410feod7kok3iublzzlk5ea5nlbn3ytfmw7vk43imoq 120"
    exit 1
fi

SUBNET_ID=$1
CHECKPOINT_PERIOD=${2:-120}

# Extract the port from the rpc URL returned by ipc-cli
PORT=$(../ipc/target/release/ipc-cli subnet rpc --network "$SUBNET_ID" | grep "rpc:" | sed -E 's/.*:([0-9]+).*/\1/')

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
