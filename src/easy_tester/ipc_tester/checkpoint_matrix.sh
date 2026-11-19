#!/bin/bash
# Checkpoint tests for several checkpoint periods and epoch lengths, one fresh devnet per cell.
# Runs on the host. `make checkpoint-matrix` sets CHECKPOINT_PERIODS, EPOCH_LENGTH_MULTIPLIERS,
# INCLUDE_EQUAL_EPOCH_LENGTH, OUT and HOST_ADDR; README.md next to this file describes them.

COMPOSE_FILE=docker-compose.local.yml

[ -n "${OUT:-}" ] || { echo "set OUT=<folder>" >&2; exit 1; }
mkdir -p "$OUT" && OUT=$(cd "$OUT" && pwd) || exit 1
cd "$(dirname "${BASH_SOURCE[0]}")/../../.." || exit 1

# $1 checkpoint period, $2 epoch length K, $3 multiplier ("-" for K equal to the period)
run_cell() {
    local period=$1 k=$2 multiplier=$3
    local cell="$OUT/P$period-K$k" start rc=0 build=0 i n ready id port token h run r t v c
    mkdir -p "$cell"
    : > "$cell/ue_scenario.out"
    start=$(date +%s)
    echo "== period $period, K $k: fresh devnet"

    docker compose -f "$COMPOSE_FILE" down -v
    rm -rf "$HOME/.ipc"
    docker ps -aq --filter name=^validator- | xargs -r docker rm -f >/dev/null
    docker network ls --format '{{.Name}}' | grep '^b4-' | xargs -r docker network rm >/dev/null
    docker image inspect fendermint:latest >/dev/null 2>&1 || build=1
    BLOCK_INTERVAL=1 UE_ACTIVATION_HEIGHT=1 docker compose -f "$COMPOSE_FILE" up -d || rc=1

    for i in $(seq 120); do
        n=$(docker exec bitcoin-ipc bitcoin-cli getblockcount 2>/dev/null)
        [ "${n:-0}" -gt 101 ] && break
        sleep 5
    done
    [ "${n:-0}" -gt 101 ] || { echo "FAIL: Bitcoin block 101 not reached after 600 s" | tee -a "$cell/ue_scenario.out"; rc=1; }

    if [ $rc -eq 0 ]; then
        for i in $(seq 60); do
            ready=1
            for id in validator1 validator2 validator3 validator4 validator5 validator6 user1 user2; do
                port=$(sed -n 's/^PROVIDER_PORT=//p' "$HOME/.ipc/$id/.env" 2>/dev/null)
                token=$(sed -n 's/^PROVIDER_AUTH_TOKEN=//p' "$HOME/.ipc/$id/.env" 2>/dev/null)
                h=$(curl -s --max-time 5 -X POST "http://127.0.0.1:$port/api" -H 'Content-Type: application/json' -H "Authorization: Bearer $token" \
                    -d '{"jsonrpc":"2.0","id":1,"method":"getconfirmedcount","params":null}' | grep -oE '"result":[0-9]+' | cut -d: -f2)
                [ "${h:-0}" -gt 101 ] || { ready=""; break; }
            done
            [ -n "$ready" ] && break
            sleep 5
        done
        [ -n "$ready" ] || { echo "FAIL: provider of $id reports no monitor height above 101 after 300 s" | tee -a "$cell/ue_scenario.out"; rc=1; }
    fi

    if [ $rc -eq 0 ]; then
        for run in "--group setup" "--group deposits" "--step join_v5" "--step traffic" "--step removal" "--step checkpoint_invariants"; do
            # shellcheck disable=SC2086
            docker exec -e HOST_ADDR="$HOST_ADDR" -e UE_CHECK_PERIOD="$period" -e UE_EPOCH_LENGTH="$k" -e UE_STEP_TIMEOUT=$((12 * k + 900)) -e UE_BUILD_IMAGE=$build \
                bitcoin-ipc ue_scenario.sh $run 2>&1 | tee -a "$cell/ue_scenario.out"
            r=${PIPESTATUS[0]}
            [ $rc -ne 0 ] || rc=$r
            build=0
        done
    fi

    cp -r "$HOME/.ipc/logs" "$HOME/.ipc/test-round" "$cell/"
    for c in bitcoin-ipc $(docker ps -a --format '{{.Names}}' | grep '^validator-'); do
        docker logs "$c" > "$cell/$c.docker.log" 2>&1
    done
    printf '%s\t%s\t%s' "$period" "$k" "$multiplier" >> "$OUT/matrix.tsv"
    for t in PASS FAIL OUT_OF_SCOPE DEFERRED; do
        v=$(grep -P "^$t\tTOTAL\t[0-9]+$" "$cell/ue_scenario.out" | tail -n 1 | cut -f3)
        printf '\t%s' "${v:-?}" >> "$OUT/matrix.tsv"
    done
    printf '\t%s\t%ss\n' "$rc" $(($(date +%s) - start)) >> "$OUT/matrix.tsv"
}

printf 'period\tK\tmultiple\tPASS\tFAIL\tOUT_OF_SCOPE\tDEFERRED\texit\tduration\n' > "$OUT/matrix.tsv"
for period in $CHECKPOINT_PERIODS; do
    for multiplier in $EPOCH_LENGTH_MULTIPLIERS; do
        run_cell "$period" $((multiplier * period + 15)) "$multiplier"
    done
    [ "$INCLUDE_EQUAL_EPOCH_LENGTH" != 1 ] || run_cell "$period" "$period" -
done
column -t -s "$(printf '\t')" "$OUT/matrix.tsv"
awk -F'\t' 'NR > 1 && $8 != 0 {f = 1} END {exit f}' "$OUT/matrix.tsv"
