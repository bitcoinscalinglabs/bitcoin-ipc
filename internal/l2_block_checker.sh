#!/bin/bash

# Check if subnet ID is provided
if [ -z "$1" ]; then
    echo "Error: Subnet ID is required"
    echo "Usage: $0 <subnet-id>"
    echo "Example: $0 /b4/t410feod7kok3iublzzlk5ea5nlbn3ytfmw7vk43imoq"
    exit 1
fi

SUBNET_ID=$1

# Extract the port from the rpc URL returned by ipc-cli
PORT=$(../ipc/target/release/ipc-cli subnet rpc --network "$SUBNET_ID" | grep "rpc:" | sed -E 's/.*:([0-9]+).*/\1/')

echo "PORT: $PORT"

while true; do
     echo "Subnet block $(cast block-number --rpc-url http://localhost:$PORT)"
     sleep 1
done
