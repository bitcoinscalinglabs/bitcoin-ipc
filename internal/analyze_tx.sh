#!/bin/bash

TXID=$1

if [ -z "$TXID" ]; then
    echo "Usage: $0 <txid>"
    exit 1
fi

# Get full decoded transaction
tx_json=$(bitcoin-cli -regtest getrawtransaction "$TXID" true)

# --- Calculate total input value ---
total_in=0
inputs=$(echo "$tx_json" | jq -c '.vin[]')

for input in $inputs; do
    prev_txid=$(echo "$input" | jq -r '.txid')
    vout_index=$(echo "$input" | jq -r '.vout')

    prev_tx=$(bitcoin-cli -regtest getrawtransaction "$prev_txid" true)
    value=$(echo "$prev_tx" | jq ".vout[] | select(.n == $vout_index) | .value")

    total_in=$(echo "$total_in + $value" | bc)
done

# --- Calculate total output value ---
total_out=$(echo "$tx_json" | jq '[.vout[].value] | add')

# --- Calculate fee ---
fee=$(echo "$total_in - $total_out" | bc)

# --- Get transaction size in virtual bytes ---
vsize=$(echo "$tx_json" | jq '.vsize')

# --- Calculate fee rate in sat/vB ---
# Convert BTC fee to satoshis (1 BTC = 100,000,000 sats)
fee_sats=$(echo "$fee * 100000000" | bc | awk '{print int($1)}')
feerate=$(echo "$fee_sats / $vsize" | bc)

# --- Print results ---
echo "Transaction:  $TXID"
echo "Total Input:  $total_in BTC"
echo "Total Output: $total_out BTC"
echo "Fee:          $fee BTC"
echo "VSize:        $vsize vB"
echo "Fee Rate:     $feerate sat/vB"
