#!/usr/bin/env bash
INTERVAL_SECONDS="${INTERVAL_SECONDS:-20}"

while true; do
    if height="$(bitcoin-cli -regtest getblockcount 2>/dev/null)"; then
        echo "Regtest height: ${height}"
        sleep "${INTERVAL_SECONDS}"
    else
        echo "Regtest height: unavailable (bitcoin-cli not ready?)"
        sleep 2
    fi
done