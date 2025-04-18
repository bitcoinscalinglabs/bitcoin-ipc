#!/bin/bash
PORT=${1:-8545}

while true; do
     echo "Subnet block $(cast block-number --rpc-url http://localhost:$PORT)"
     sleep 1
done
