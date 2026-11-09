#!/bin/bash
set -Eeuo pipefail
shopt -s inherit_errexit

# Unilateral-exit integration scenario for the local devnet.

EPOCH_LENGTH=${UE_EPOCH_LENGTH:-90} # K, Bitcoin blocks; also exercise K=P in the matrix
EPOCHS_BOOTSTRAP=107  # epochs: the block duration scales with K (9630 blocks at the default)
EPOCHS_V5=6           # epochs: 6K blocks seated, followed by 3K blocks to maturity
POWER_UNIT_EXPONENT=6 # one power unit is 1,000,000 sat; collateral below has power 100
MIN_TIMEOUT=1
MAX_TIMEOUT=1000     # epochs, so the permitted timeout in Bitcoin blocks scales with K
MIN_DEPOSIT=330
KILL_VOTE_PERIOD=36
KILL_DELAY_PERIOD=5
GATEWAY=0x77aa40b105843728088c0132e43fc44348881da8
COLLATERAL=100000000  # sats, the minimum validator stake below
MIN_VALIDATOR_STAKE=100000000
MIN_VALIDATORS=4
CHECK_PERIOD=${UE_CHECK_PERIOD:-30} # L2 blocks per periodic checkpoint; batches can checkpoint earlier
DEPOSIT=50000000      # sats per deposit
WITHDRAWAL=10000000   # sats released from the L2 while validator 5 is a member
VALIDATOR_FUND=100000000 # sats deposited after bootstrap; credited when parent finality reaches the deposit's Bitcoin block
CHECKPOINT_TIMEOUT=630   # recomputed as 7K after loading the saved scenario parameters
MONITOR_TIMEOUT=60       # seconds for the monitor to process an already-mined block (2 s polling, 30 s error backoff)
OUTAGE_STABLE_SECONDS=20 # seconds with no target-node progress while its peers keep committing
IP_PREFIX="66.222.44.55:808"   # validator n advertises ${IP_PREFIX}$((n - 1))

PARENT="/b4"
LOGS=/root/.ipc/logs
VAL1_API_URL="http://127.0.0.1:3030/api"
VAL1_BEARER_TOKEN="validator1_auth_token"
export RUST_LOG=${RUST_LOG_CLI:-info}   # ipc-cli logs the txids at info

declare -A BACKUP JOIN_TXID COMMIT_TXID
T5=""
V5_REMOVAL=""
V5_CONFIG=""
V5_CP=""
SUBNET=""
CHECKS=()
STOPPED_PROVIDER_ENV=""
PAUSED_MONITOR_PID=""
STOPPED_L2_NODE=""
OUTAGE_LOG_NODE=""
OUTAGE_LOG_SINCE=""
OUTAGE_LOG_LABEL=""
PAIR_A=""
PAIR_B=""
STATE_DIR=${UE_STATE_DIR:-/root/.ipc/test-round/ue}
STEP_CHECKS=""
SIGN_RECORD_READER=${UE_SIGN_RECORD_READER:-/tmp/dump-eots-votes}

# ---------------------------------------------------------------- helpers

say() { echo; echo "### $*"; }

fail() {
  echo "FAILED: $*" >&2
  [ -z "$STEP_CHECKS" ] || printf 'FAIL\t%s\n' "$*" >> "$STEP_CHECKS"
  exit 1
}

# check <description> <command...>: the command's exit status is the check
check() {
  local what="$1"; shift
  if "$@"; then
    echo "ok: $what"; CHECKS+=("ok: $what")
  else
    echo "FAILED: $what" >&2
    printf 'FAIL\t%s\n' "$what" >> "$STEP_CHECKS"
  fi
}

# A safety or fixture prerequisite aborts only its own step.
require_check() {
  local what="$1"; shift
  if "$@"; then echo "ok: $what"; CHECKS+=("ok: $what"); else fail "$what"; fi
}

# expect_fail <description> <command...>: the command must fail; its last error line is printed
expect_fail() {
  local what="$1"; shift
  local out
  if out=$("$@" 2>&1); then
    echo "$out"
    echo "FAILED: $what (the command succeeded)" >&2
    printf 'FAIL\t%s (the command succeeded)\n' "$what" >> "$STEP_CHECKS"
    return 0
  fi
  echo "   refused: $(echo "$out" | grep -i -m1 'error\|refused\|rejected\|must\|not ' || echo "$out" | tail -1)"
  echo "ok: $what"; CHECKS+=("ok: $what")
}

# cli <n|user1|user2> <ipc-cli args...>: runs ipc-cli with that actor's config, printing the command
cli() {
  local who="$1"; shift
  case "$who" in [1-6]) who="validator$who" ;; esac
  echo "+ ipc-cli --config-path /root/.ipc/$who/config.toml $*"
  ipc-cli --config-path "/root/.ipc/$who/config.toml" "$@"
}

# rpc <method> <params json>: JSON-RPC to validator1's provider, fails on a JSON-RPC error
rpc() {
  local resp
  resp=$(curl -sS --fail-with-body --connect-timeout 2 --max-time 15 -X POST "$VAL1_API_URL" \
    -H "Content-Type: application/json" -H "Authorization: Bearer $VAL1_BEARER_TOKEN" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}") || return 1
  if ! echo "$resp" | jq -e 'type == "object" and has("result") and .error == null' >/dev/null 2>&1; then
    echo "provider $1: $resp" >&2
    return 1
  fi
  printf '%s' "$resp"
}

getsubnet() { rpc getsubnet "{\"subnet_id\":\"$SUBNET\"}"; }
committee_size() { getsubnet | jq '.result.committee.validators | length'; }
waiting_size() { getsubnet | jq '.result.waiting_committee.validators // [] | length'; }
config_number() { getsubnet | jq '.result.committee.configuration_number'; }
cp_number() { getsubnet | jq '.result.last_checkpoint_number // 0'; }
cp_txid() { rpc getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"number\":$1}" | jq -r '.result.txid'; }

# Check identities as well as size, without requiring a transient waiting committee.
committee_is() {
  local n expected keys=()
  for n in "$@"; do keys+=("$(xonly "$n")"); done
  expected=$(jq -cn --args '$ARGS.positional | sort' "${keys[@]}")
  getsubnet | jq -e --argjson keys "$expected" \
    '([.result.committee.validators[].pubkey] | sort) == $keys' >/dev/null
}

# Before bootstrap getsubnet fails; "none" keeps that expected error out of the ERR trap.
progress() { echo "checkpoint $(cp_number 2>/dev/null || echo none), tip $(tip)"; }
# wait_for <description> <exclusive Bitcoin tip deadline, or 0> <condition...>.
wait_for() {
  local what="$1" deadline="$2" started=$SECONDS; shift 2
  while true; do
    if "$@"; then break; fi
    if [ "$deadline" -gt 0 ] && [ "$(tip)" -ge "$deadline" ]; then
      fail "$what before tip $deadline ($(progress))"
    fi
    [ "$((SECONDS - started))" -lt "$CHECKPOINT_TIMEOUT" ] || fail "$what after $CHECKPOINT_TIMEOUT s ($(progress))"
    sleep 1
  done
  echo "   $what ($(progress))"
}

tip() { bitcoin-cli getblockcount; }
# Wait for both mining and indexing; bitcoind's tip can be ahead of the monitor.
wait_block() {
  local target="$1" started=$SECONDS height
  echo "   waiting for block $target (tip $(tip))"
  while [ "$(tip)" -lt "$target" ]; do
    [ "$((SECONDS - started))" -lt "$CHECKPOINT_TIMEOUT" ] || fail "Bitcoin did not reach block $target after $CHECKPOINT_TIMEOUT s"
    sleep 0.5
  done
  echo "   waiting for the monitor to process block $target"
  started=$SECONDS
  while true; do
    height=$(rpc getconfirmedcount 'null' | jq -er '.result | numbers') || fail "cannot read the monitor's processed height"
    [ "$height" -ge "$target" ] && break
    [ "$((SECONDS - started))" -lt "$MONITOR_TIMEOUT" ] || fail "monitor at block $height, still waiting for $target after $MONITOR_TIMEOUT s"
    sleep 0.5
  done
}
wait_blocks() { wait_block $(( $(tip) + $1 )); }
# wait_checkpoints <k>: until k more checkpoints are recorded by the monitor
wait_checkpoints() {
  local target=$(( $(cp_number) + $1 )) started=$SECONDS
  echo "   waiting for checkpoint number $target (now $(cp_number), about one per 60 s)"
  while [ "$(cp_number)" -lt "$target" ]; do
    [ "$((SECONDS - started))" -lt "$CHECKPOINT_TIMEOUT" ] || fail "no checkpoint $target after $CHECKPOINT_TIMEOUT s"
    sleep 1
  done
  echo "   checkpoint $target recorded at tip $(tip)"
}
# wait_rotation <epoch> <cp_before> <deadline>: wait for that epoch's flagged checkpoint.
wait_rotation() {
  wait_for "rotation of epoch $1" "$3" rotation_recorded "$1" "$2"
}
# rotation_cp_after <cp_before> <epoch>: checkpoint evidence survives the waiting state.
rotation_cp_after() {
  local n
  for n in $(seq $(( $1 + 1 )) "$(cp_number)"); do
    if rpc getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"number\":$n}" \
        | jq -e --argjson epoch "$2" '.result.is_rotation_checkpoint and .result.epoch == $epoch' >/dev/null; then
      echo "$n"; return
    fi
  done
}
rotation_recorded() {
  [ -n "$(rotation_cp_after "$2" "$1")" ]
}

evm() { local v="IPC_ADDRESS_OF_VALIDATOR_$1"; echo "${!v}"; }
# x-only key of validator n: the EVM keystore key, uncompressed "04 X Y" from ipc-cli, X = 64 hex chars
xonly() { cli "$1" wallet pub-key --wallet-type evm --address "$(evm "$1")" | tail -1 | cut -c3-66; }
new_btc_address() { bitcoin-cli -rpcwallet=default getnewaddress; }
received() { bitcoin-cli -rpcwallet=default getreceivedbyaddress "$1" 1 | awk '{printf "%.0f", $1 * 100000000}'; }
list_validators() { cli 1 subnet list-validators --subnet "$SUBNET"; }

l2_rpc() {
  curl -sS --fail-with-body --connect-timeout 2 --max-time 15 "http://${HOST_ADDR:-host.docker.internal}:$1/" \
    -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}" \
    | jq -er '.result | strings'
}
l2_balance() { l2_rpc 8545 eth_getBalance "[\"$1\",\"latest\"]"; }
v5_synced() {
  local head1 head5
  head1=$(l2_rpc 8545 eth_blockNumber '[]') || return 1
  head5=$(l2_rpc 8945 eth_blockNumber '[]') || return 1
  python3 - "$head1" "$head5" <<'PY'
import sys
head1, head5 = (int(h, 16) for h in sys.argv[1:])
sys.exit(0 if head1 > 0 and head5 >= head1 - 2 else 1)
PY
}
balance_is() {
  local actual; actual=$(l2_balance "$1") || return 1
  python3 - "$actual" "$2" <<'PY'
import sys
sys.exit(0 if int(sys.argv[1], 16) == int(sys.argv[2]) else 1)
PY
}
wait_deposit() {
  local expected
  expected=$(python3 - "$2" "$3" <<'PY'
import sys
print(int(sys.argv[1], 16) + int(sys.argv[2]) * 10**10)
PY
)
  wait_for "deposit of $3 sats credited to $1" 0 balance_is "$1" "$expected"
}
received_is() { [ "$(received "$1")" -eq "$2" ]; }
container_running() { [ "$(docker inspect --format '{{.State.Running}}' "$1")" = true ]; }

# Raw replies retain JSON-RPC errors for rejection checks and use bounded HTTP requests.
provider_reply() {
  local n="$1" method="$2" params="$3" subnet="${4:-a}" base=3029
  [ "$subnet" != b ] || base=3129
  curl -fsS --connect-timeout 2 --max-time 15 "http://127.0.0.1:$((base + n))/api" \
    -H 'Content-Type: application/json' -H "Authorization: Bearer validator${n}_auth_token" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}"
}
provider_ready() { provider_reply "$1" getconfirmedcount null | jq -e '.error == null and (.result | type == "number")' >/dev/null; }

comet_result() {
  local n="$1"; shift
  curl -fsS --connect-timeout 2 --max-time 5 \
    "http://${HOST_ADDR:-host.docker.internal}:$((26657 + (n - 1) * 100))/$*" \
    | jq -e 'select(.error == null and .result != null) | .result'
}
comet_height() { comet_result "$1" status | jq -er '.sync_info.latest_block_height | tonumber'; }
node_at_height() { local h; h=$(comet_height "$1") || return 1; [ "$h" -ge "$2" ]; }

# StartedAt and RestartCount detect a crash even when Docker has already restarted the process.
node_processes() {
  docker inspect --format '{{.Name}} {{.State.Running}} {{.RestartCount}} {{.State.StartedAt}}' \
    "validator-$1-fendermint" "validator-$1-cometbft"
}
node_processes_unchanged() {
  local current
  current=$(node_processes "$1") || return 1
  [[ "$current" != *' false '* ]] && [ "$current" = "$(cat "$2")" ]
}
component_pid() {
  local pid
  pid=$(pgrep -f "^$1 --env /root/\.ipc/validator$2/\.env$") || return 1
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  echo "$pid"
}

# Restore components deliberately interrupted by the scenario, including on failure.
restore_outage_components() {
  local status="$1" svc restored=0
  if [ "$status" -ne 0 ] && [ -n "$OUTAGE_LOG_NODE" ]; then
    for svc in fendermint cometbft; do
      docker logs --timestamps --since "$OUTAGE_LOG_SINCE" "validator-$OUTAGE_LOG_NODE-$svc" \
        > "$LOGS/$OUTAGE_LOG_LABEL-$svc.log" 2>&1 || true
      grep -iE 'error|warn|fail|panic|replay' "$LOGS/$OUTAGE_LOG_LABEL-$svc.log" | tail -20 >&2 || true
    done
  fi
  if [ -n "$PAUSED_MONITOR_PID" ]; then kill -CONT "$PAUSED_MONITOR_PID" || restored=1; fi
  if [ -n "$STOPPED_PROVIDER_ENV" ]; then
    if ! provider_ready 4 >/dev/null 2>&1; then
      provider --env "$STOPPED_PROVIDER_ENV" >> "$LOGS/provider-validator4.log" 2>&1 &
    fi
    local until=$((SECONDS + 60))
    until provider_ready 4 >/dev/null 2>&1; do
      if [ "$SECONDS" -ge "$until" ]; then restored=1; break; fi
      sleep 2
    done
  fi
  if [ -n "$STOPPED_L2_NODE" ]; then
    docker start "validator-$STOPPED_L2_NODE-fendermint" \
      "validator-$STOPPED_L2_NODE-cometbft" "validator-$STOPPED_L2_NODE-ethapi" >/dev/null || restored=1
  fi
  return "$restored"
}

quorum_without() {
  local address
  address=$(comet_result "$1" status | jq -er '.validator_info.address') || return 1
  comet_result 2 validators | jq -e --arg excluded "$address" \
    '(.validators | length == 4) and any(.validators[]; .address == $excluded)
     and (([.validators[].voting_power | tonumber] | add) as $total
       | ([.validators[] | select(.address != $excluded) | .voting_power | tonumber] | add)
         >= ((2 * $total / 3 | floor) + 1))' >/dev/null
}

# Devnet peer requirement: all earlier validators connect, and each active validator has multiple peers.
peers_connected() {
  local n earlier id peers
  for n in 1 2 3 4; do
    peers=$(comet_result "$n" net_info) || return 1
    jq -e '(.n_peers | tonumber) >= 2' <<< "$peers" >/dev/null || return 1
    for ((earlier=1; earlier<n; earlier++)); do
      id=$(comet_result "$earlier" status | jq -er '.node_info.id') || return 1
      jq -e --arg id "$id" 'any(.peers[]; .node_info.id == $id)' <<< "$peers" >/dev/null || return 1
    done
  done
}

# Observe the affected node staying alive while the remaining nodes advance.
observe_outage() {
  local label="$1" cp_before="$2" waited=0 stable=0 last=-1 height reference start
  start=$(comet_height 1)
  while [ "$waited" -lt "$CHECKPOINT_TIMEOUT" ]; do
    node_processes_unchanged 4 "$LOGS/$label-processes-before.txt" \
      || fail "$label: validator4's fendermint or CometBFT stopped or restarted"
    height=$(comet_height 4) || fail "$label: validator4's CometBFT no longer answers status"
    reference=$(comet_height 1)
    if [ "$height" -eq "$last" ]; then stable=$((stable + 5)); else stable=0; fi
    if [ "$stable" -ge "$OUTAGE_STABLE_SECONDS" ] && [ "$reference" -ge "$((start + CHECK_PERIOD))" ] \
        && [ "$reference" -gt "$((height + 2))" ] && checkpoint_past "$cp_before" "$start"; then
      echo "   $label: validator4 paused at $height for $stable s; validator1 at $reference; checkpoint $(cp_number)"
      CHECKS+=("ok: $label pauses validator4 without a crash while the quorum produces blocks and checkpoints")
      return
    fi
    last="$height"
    sleep 5; waited=$((waited + 5))
  done
  fail "$label: no live paused validator4 with advancing peers and checkpoints after $CHECKPOINT_TIMEOUT s"
}

eots_status() {
  comet_result "$1" 'abci_query?path=%22%2Feots%22' \
    | jq -er '.response | select((.code // 0 | tonumber) == 0) | .value | @base64d | fromjson'
}
recorded_height_reached() {
  eots_status "$1" | jq -e --argjson h "$2" \
    '.recorded_final_height >= $h and .final_height >= .recorded_final_height' >/dev/null
}

# H+1's header commits the application state after H; compare a fixed block range on all validators.
same_committed_blocks() {
  local reference="$1" from="$2" to="$3" h n expected actual
  for ((h=from; h<=to; h++)); do
    expected=$(comet_result "$reference" "block?height=$h" \
      | jq -cer --argjson h "$h" 'select((.block.header.height | tonumber) == $h)
        | [.block_id.hash, .block.header.app_hash] | select(all(.[]; type == "string" and length > 0))') || return 1
    for n in 1 2 3 4; do
      actual=$(comet_result "$n" "block?height=$h" | jq -c '[.block_id.hash, .block.header.app_hash]') || return 1
      [ "$actual" = "$expected" ] || { echo "height $h: validator$n $actual, reference $expected" >&2; return 1; }
    done
  done
}
l2_block_state() {
  local hex; printf -v hex '0x%x' "$2"
  curl -fsS --connect-timeout 2 --max-time 10 \
    "http://${HOST_ADDR:-host.docker.internal}:$((8545 + ($1 - 1) * 100))/" \
    -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$hex\",false]}" \
    | jq -cer 'select(.error == null) | .result | [.hash, .stateRoot, .receiptsRoot, .transactionsRoot]
      | select(all(.[]; type == "string" and length > 0))'
}
same_l2_state() {
  local expected actual
  expected=$(l2_block_state "$2" "$3") || return 1
  actual=$(l2_block_state "$1" "$3") || return 1
  [ "$actual" = "$expected" ]
}
check_recovered_node() {
  local n="$1" reference="$2" from="$3" label="$4" target recorded v cp_before
  cp_before=$(cp_number); target=$(comet_height "$reference")
  for v in 1 2 3 4; do wait_for "validator$v reaches $((target + 1)) after $label" 0 node_at_height "$v" "$((target + 1))"; done
  check "$label: every recovered block and application hash agrees through $((target + 1))" \
    same_committed_blocks "$reference" "$from" "$((target + 1))"
  wait_for "$label: validator$n serves the same Ethereum block and state roots at $target" 0 same_l2_state "$n" "$reference" "$target"
  wait_for "$label: a fresh checkpoint commits a block beyond recovery height $target" 0 checkpoint_past "$cp_before" "$target"
  recorded=$(eots_status "$reference" | jq -er '.recorded_final_height')
  wait_for "$label: validator$n retains carried EOTS finality through $recorded" 0 recorded_height_reached "$n" "$recorded"
}

# The reader is the dump_eots_votes test executable (src/easy_tester/ipc_tester/dump_eots_votes.rs),
# which the caller builds in the container and installs as SIGN_RECORD_READER (README, Sign-record reader).
sign_record_reader_ok() {
  local out
  if [ ! -x "$SIGN_RECORD_READER" ]; then
    echo "no sign-record reader at $SIGN_RECORD_READER: build it in the bitcoin-ipc container and install it there," \
      "or set UE_SIGN_RECORD_READER (src/easy_tester/ipc_tester/README.md, Sign-record reader)" >&2
    return 1
  fi
  if ! out=$("$SIGN_RECORD_READER" --list 2>&1); then
    echo "the sign-record reader $SIGN_RECORD_READER does not start in the container: $(tail -1 <<< "$out")" \
      "(a host build links the host's newer glibc; build it in the bitcoin-ipc container)" >&2
    return 1
  fi
}
sign_record_snapshot() {
  EOTS_SIGN_RECORD_DB=/root/.ipc/validator4/eots.sk.votes.db EOTS_SIGN_RECORD_OUTPUT="$1" \
    "$SIGN_RECORD_READER" --ignored --exact dump_sign_record >/dev/null
}
sign_record_ready() {
  [ -s /root/.ipc/validator4/eots.sk.votes.db/data.mdb ] || return 1
  sign_record_snapshot "$LOGS/provider-outage-votes-ready.json" || return 1
  jq -e --arg subnet "$SUBNET" 'any(.[]; .subnet_id == $subnet)' \
    "$LOGS/provider-outage-votes-ready.json" >/dev/null
}

# Check that every complete old vote is still present, with one record per height.
sign_record_extended() {
  sign_record_snapshot "$LOGS/provider-outage-votes-after.json" || return 1
  python3 - "$LOGS/provider-outage-votes-before.json" "$LOGS/provider-outage-votes-after.json" "$SUBNET" <<'PY'
import json
import pathlib
import sys

old, records = (json.loads(pathlib.Path(p).read_text()) for p in sys.argv[1:3])
assert old, "no own votes existed before the provider stopped"
seen = set()
for record in records:
    key = (json.dumps(record["subnet_id"], sort_keys=True), record["vote"]["height"])
    assert key not in seen, f"a height has a second signed record: {key}"
    seen.add(key)
by_key = {(r["subnet_id"], r["vote"]["height"]): r for r in records}
for record in old:
    key = (record["subnet_id"], record["vote"]["height"])
    assert by_key.get(key) == record, f"a complete old signed vote was lost or changed: {key}"
before = [r["vote"]["height"] for r in old if r["subnet_id"] == sys.argv[3]]
assert before, "no own votes existed before the provider stopped"
assert any(r["subnet_id"] == sys.argv[3] and r["vote"]["height"] > max(before)
           for r in records), "the restarted provider has not signed a new height"
PY
}

capture_node_logs() {
  local n="$1" since="$2" label="$3" svc
  for svc in fendermint cometbft; do
    docker logs --timestamps --since "$since" "validator-$n-$svc" \
      > "$LOGS/$label-$svc.log" 2>&1
  done
  if grep -iE 'error on replay|panic|CONSENSUS FAILURE|wrong Block.Header|failed to apply block|failed to execute ABCI request' \
      "$LOGS/$label-fendermint.log" "$LOGS/$label-cometbft.log"; then
    fail "$label: node logs contain an execution or replay failure (including replay EOF)"
  fi
  OUTAGE_LOG_NODE=""
}
track_node_logs() { OUTAGE_LOG_NODE="$1"; OUTAGE_LOG_SINCE="$2"; OUTAGE_LOG_LABEL="$3"; }

# join_ue <n> <epochs>: validator n joins with join-ue; records its backup address and txids
join_ue() {
  local n="$1" epochs="$2" out
  BACKUP[$n]=$(new_btc_address)
  if ! out=$(cli "$n" subnet join-ue --from "$(evm "$n")" --subnet "$SUBNET" --collateral "$COLLATERAL" \
    --ip "${IP_PREFIX}$((n - 1))" --backup-address "${BACKUP[$n]}" --epochs "$epochs" 2>&1); then
    echo "$out"; fail "join-ue of validator$n"
  fi
  echo "$out"
  # The provider client logs the txid; the command's documented stdout gives the monitor height.
  JOIN_TXID[$n]=$(echo "$out" | grep -o 'Joined subnet with txid: [0-9a-f]*' | awk '{print $NF}')
  [ -n "${JOIN_TXID[$n]}" ] || fail "join-ue of validator$n: no txid in the output"
  wait_for "validator$n accepted join appears in the monitor" 0 has_join_key "$n"
  COMMIT_TXID[$n]=$(bitcoin-cli getrawtransaction "${JOIN_TXID[$n]}" 1 | jq -r '.vin[0].txid')
  echo "   validator$n: join txid ${JOIN_TXID[$n]}, collateral output ${COMMIT_TXID[$n]}:1, backup ${BACKUP[$n]}"
}

# collateral_unspent <n>: the collateral output of validator n's last join-ue is unspent on chain
collateral_unspent() { [ "$(bitcoin-cli gettxout "${COMMIT_TXID[$1]}" 1 false | jq -r '.value // empty')" != "" ]; }
collateral_spent() {
  local output
  output=$(bitcoin-cli gettxout "${COMMIT_TXID[$1]}" 1 false) || return 1
  [ -z "$output" ] || [ "$output" = null ]
}

# Read the committed fields from the chain without the Rust encoders.
join_commitment_from_chain() {
  local n="$1"
  python3 - "${JOIN_TXID[$n]}" "$SUBNET" "${BACKUP[$n]}" "$(xonly "$n")" <<'PY'
import json
import subprocess
import sys

txid, subnet, backup, owner = sys.argv[1:]
def btc(*args):
    return json.loads(subprocess.check_output(["bitcoin-cli", *args], text=True))

tx = btc("getrawtransaction", txid, "1")
witness = [bytes.fromhex(x) for x in tx["vin"][0]["txinwitness"]]
if witness[-1][:1] == b"\x50":
    witness.pop()
leaf = witness[-2]
chunks, p = [], 0
while p < len(leaf) - 1:
    op = leaf[p]
    p += 1
    if op <= 75:
        size = op
    elif op in (76, 77, 78):
        width = {76: 1, 77: 2, 78: 4}[op]
        size = int.from_bytes(leaf[p:p+width], "little")
        p += width
    elif 81 <= op <= 96 or op == 79:
        chunks.append(bytes([op - 80 if op != 79 else 129]))
        assert leaf[p] == 0x75, "missing OP_DROP"
        p += 1
        continue
    else:
        raise AssertionError("unexpected data-leaf opcode")
    assert p + size < len(leaf), "truncated data push"
    chunks.append(leaf[p:p+size])
    p += size
    assert leaf[p] == 0x75, "missing OP_DROP"
    p += 1
assert leaf[p:] == b"\x51", "missing final OP_TRUE"
data = b"".join(chunks)
p = 0
def take(size):
    global p
    value = data[p:p+size]
    assert len(value) == size, "truncated join payload"
    p += size
    return value
def varint(bits):
    value, shift, encoded = 0, 0, []
    while True:
        byte = take(1)[0]
        encoded.append(byte)
        value |= (byte & 127) << shift
        if byte < 128:
            break
        shift += 7
        assert shift < bits, "overflowing varint"
    assert value < 1 << bits, "out-of-range varint"
    assert len(encoded) == 1 or encoded[-1] != 0, "non-minimal varint"
    return value
def string():
    return take(varint(64)).decode("utf-8")

assert take(6) == b"IPCJUE"
assert string() == subnet
ip_kind = take(1)[0]
assert ip_kind in (0, 1)
take(4 if ip_kind == 0 else 16)
varint(16)
assert string() == backup
pubkey = take(32).hex()
assert pubkey == owner
timeout = varint(32)
pk_eots = take(32).hex()
start, count, root = varint(64), varint(32), take(32).hex()
take(64)  # nonce commitment signature
take(64)  # validator signature
assert p == len(data), "trailing join bytes"
assert pk_eots != pubkey
assert start >= 1 and count >= 1
height = btc("getblockheader", tx["blockhash"])["height"]
print(json.dumps(dict(pubkey=pubkey, pk_eots=pk_eots, start=start, count=count,
                     root=root, block_height=height, timeout=timeout)))
PY
}

check_commitment_query() {
  local expected keys commitments eots
  expected=$(join_commitment_from_chain "$1") || return 1
  keys=$(rpc geteotskeys "{\"subnet_id\":\"$SUBNET\"}") || return 1
  echo "$keys" | jq -e --argjson e "$expected" \
    '[.result[] | select(.pubkey == $e.pubkey and .pk_eots == $e.pk_eots)] | length == 1' >/dev/null || return 1
  eots=$(echo "$expected" | jq -r .pk_eots)
  commitments=$(rpc geteotscommitments "{\"subnet_id\":\"$SUBNET\",\"pk_eots\":\"$eots\"}") || return 1
  echo "$commitments" | jq -e --argjson e "$expected" \
    '.result | length == 1 and .[0] == ($e | {start, count, root, block_height})' >/dev/null
}

v5_is_slashable() {
  [ "$(tip)" -lt "$V5_REMOVAL" ] && collateral_unspent 5 || return 1
  rpc getunslashableoutputs "{\"subnet_id\":\"$SUBNET\"}" \
    | jq -e --arg txid "${COMMIT_TXID[5]}" \
      '(.result | type == "array") and all(.result[]; .txid != $txid or .vout != 1)' >/dev/null
}

timeout_of() { join_commitment_from_chain "$1" | jq -er '.timeout'; }

has_join_key() {
  local key; key=$(xonly "$1") || return 1
  rpc geteotskeys "{\"subnet_id\":\"$SUBNET\"}" | jq -e --arg key "$key" '.result | any(.pubkey == $key)' >/dev/null
}

# add_subnet_to_configs: the child subnet's L2 endpoint in every ipc-cli config (as the bootstrap script does)
add_subnet_to_configs() {
  local f port
  for f in /root/.ipc/config.toml /root/.ipc/validator{1,2,3,4,5}/config.toml /root/.ipc/user{1,2}/config.toml; do
    case "$f" in
      *validator1*) port=8545 ;; *validator2*) port=8645 ;; *validator3*) port=8745 ;;
      *validator4*) port=8845 ;; *validator5*) port=8945 ;; *) port=8545 ;;
    esac
    cat >> "$f" <<EOF

# Subnet A (unilateral-exit scenario). Created at height $(tip)
[[subnets]]
id = "${SUBNET}"

[subnets.config]
network_type = "fevm"
provider_http = "http://${HOST_ADDR:-host.docker.internal}:${port}/"
gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
EOF
  done
}

# ---------------------------------------------------------------- steps

step_create() {
  say "1. create the subnet (validator1), validators 1-6 whitelisted"
  [ -n "${UE_ACTIVATION_HEIGHT:-}" ] || fail "UE_ACTIVATION_HEIGHT is not set in the container (start with UE_ACTIVATION_HEIGHT=1 make local-up)"
  local whitelist out
  whitelist=$(for n in 1 2 3 4 5 6; do xonly "$n"; done | paste -sd,)
  out=$(cli 1 subnet create --parent "$PARENT" --min-validators "$MIN_VALIDATORS" \
    --bottomup-check-period "$CHECK_PERIOD" btc --min-validator-stake "$MIN_VALIDATOR_STAKE" \
    --power-unit-exponent "$POWER_UNIT_EXPONENT" --validator-whitelist "$whitelist" \
    --epoch-length "$EPOCH_LENGTH" --min-timeout "$MIN_TIMEOUT" --max-timeout "$MAX_TIMEOUT" \
    --min-deposit "$MIN_DEPOSIT" --kill-vote-period "$KILL_VOTE_PERIOD" --kill-delay-period "$KILL_DELAY_PERIOD" 2>&1)
  echo "$out"
  SUBNET=$(echo "$out" | grep -o 'created subnet with id: [^ ]*' | awk '{print $NF}')
  [ -n "$SUBNET" ] || fail "create: no subnet id in the output"
  echo "SUBNET=$SUBNET"
  wait_blocks 1
  # the subnet state exists only once bootstrapped; before that the monitor holds the genesis info
  check "subnet $SUBNET known to the monitor" rpc getgenesisinfo "{\"subnet_id\":\"$SUBNET\"}"
  local genesis; genesis=$(rpc getgenesisinfo "{\"subnet_id\":\"$SUBNET\"}")
  check "all seven epoch flags reach the monitor with their requested values" \
    jq -e --argjson p "$POWER_UNIT_EXPONENT" --argjson k "$EPOCH_LENGTH" \
      --argjson min "$MIN_TIMEOUT" --argjson max "$MAX_TIMEOUT" --argjson deposit "$MIN_DEPOSIT" \
      --argjson vote "$KILL_VOTE_PERIOD" --argjson delay "$KILL_DELAY_PERIOD" \
      '.result.create_subnet_msg | .power_unit_exponent == $p and .epoch_length == $k
       and .min_timeout == $min and .max_timeout == $max and .min_deposit == $deposit
       and .kill_vote_period == $vote and .kill_delay_period == $delay' <<< "$genesis"
}

step_bootstrap() {
  say "2. bootstrap: validators 1-4 join with join-ue, seat for $EPOCHS_BOOTSTRAP epochs"
  expect_fail "join-ue refuses another validator's --from key" cli 1 subnet join-ue --from "$(evm 5)" \
    --subnet "$SUBNET" --collateral "$COLLATERAL" --ip "${IP_PREFIX}4" \
    --backup-address "$(new_btc_address)" --epochs "$EPOCHS_BOOTSTRAP"
  for epochs in 0 "$((MAX_TIMEOUT + 1))"; do
    expect_fail "join-ue with out-of-range --epochs $epochs refused" cli 5 subnet join-ue --from "$(evm 5)" --subnet "$SUBNET" \
      --collateral "$COLLATERAL" --ip "${IP_PREFIX}4" --backup-address "$(new_btc_address)" --epochs "$epochs"
  done
  for n in 1 2 3 4; do join_ue "$n" "$EPOCHS_BOOTSTRAP"; done
  wait_blocks 2
  list_validators
  check "committee of 4" [ "$(committee_size)" -eq 4 ]
  for n in 1 2 3 4; do check "collateral output of validator$n unspent on chain" collateral_unspent "$n"; done
  add_subnet_to_configs
  say "2c. deposit to validators 1-5; the L2 credits each when parent finality reaches its Bitcoin block"
  for n in 1 2 3 4 5; do cli user1 cross-msg fund --subnet "$SUBNET" btc --to "$(evm "$n")" "$VALIDATOR_FUND"; done
  wait_blocks 1
}

step_spin_up() {
  say "3. spin up the L2, including validator 5's syncing node"
  # local-build builds bitcoin-ipc and ipc-cli; spin-up reuses any existing fendermint image.
  if [ "${UE_BUILD_IMAGE:-1}" = 1 ]; then
    (cd /workspace/ipc/fendermint && make docker-build)
  else
    docker image inspect fendermint:latest >/dev/null || fail "build fendermint before setting UE_BUILD_IMAGE=0"
  fi
  echo "+ spin_up_subnet_a_from_container.sh $SUBNET"
  spin_up_subnet_a_from_container.sh "$SUBNET" | tee "$LOGS/spin-up-subnet-a.log"
  # the validator5 mode of the spin-up script needs the two ids it printed
  export CometBftID; CometBftID=$(grep -m1 '^export CometBftID=' "$LOGS/spin-up-subnet-a.log" | cut -d= -f2)
  export ResolverAddress; ResolverAddress=$(grep -m1 '^export ResolverAddress=' "$LOGS/spin-up-subnet-a.log" | cut -d= -f2)
  [ -n "$CometBftID" ] && [ -n "$ResolverAddress" ] || fail "the spin-up did not print CometBftID and ResolverAddress"
  echo "+ spin_up_subnet_a_from_container.sh $SUBNET validator5"
  spin_up_subnet_a_from_container.sh "$SUBNET" validator5 | tee "$LOGS/spin-up-subnet-a-validator-5.log"
  for svc in fendermint cometbft ethapi; do
    check "validator-5-$svc running" container_running "validator-5-$svc"
  done
  wait_for "validator5's node caught up before its join" 0 v5_synced
}

step_deposits() {
  say "4. deposits by users 1 and 2 are credited when parent finality reaches their Bitcoin blocks"
  local before1 before2 bitcoin_from subnet_from
  bitcoin_from=$(rpc getconfirmedcount null | jq -er '.result')
  subnet_from=$(l2_rpc 8545 eth_blockNumber '[]')
  before1=$(l2_balance "$IPC_ADDRESS_OF_USER_1"); before2=$(l2_balance "$IPC_ADDRESS_OF_USER_2")
  cli user1 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_1" "$DEPOSIT"
  cli user2 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_2" "$DEPOSIT"
  wait_blocks 1
  touch "$STATE_DIR/deposits_sent.ready"
  wait_deposit "$IPC_ADDRESS_OF_USER_1" "$before1" "$DEPOSIT"
  wait_deposit "$IPC_ADDRESS_OF_USER_2" "$before2" "$DEPOSIT"
  # Check the deposit is recorded in its own Bitcoin block, including in epoch 0.
  python3 - "${HOST_ADDR:-host.docker.internal}" "$GATEWAY" "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" \
    "$SUBNET" "$bitcoin_from" "$subnet_from" "$IPC_ADDRESS_OF_USER_1" "$IPC_ADDRESS_OF_USER_2" \
    "$before1" "$before2" "$DEPOSIT" <<'PY'
import base64
import json
import subprocess
import sys
import time
import urllib.request

host, gateway, provider, token, subnet, bitcoin_from, subnet_from, address1, address2, before1, before2, sats = sys.argv[1:]
def rpc(url, method, params, bearer=None):
    headers = {"Content-Type": "application/json"}
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    request = urllib.request.Request(url, json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=params)).encode(), headers)
    with urllib.request.urlopen(request, timeout=15) as response:
        reply = json.load(response)
    assert reply.get("error") is None, reply
    return reply["result"]
def eth(method, params):
    return rpc(f"http://{host}:8545/", method, params)
def evm_address(address):
    if address.startswith("0x"):
        return address.lower()
    encoded = address[5:].upper()
    return "0x" + base64.b32decode(encoded + "=" * (-len(encoded) % 8))[:20].hex()

selector = subprocess.check_output(["cast", "sig", "getLatestParentFinality()"], text=True).strip()
def finality(height):
    data = eth("eth_call", [dict(to=gateway, data=selector), hex(height)])
    return int(data[2:66], 16)

messages = []
processed = rpc(provider, "getconfirmedcount", None, token)
for height in range(int(bitcoin_from) + 1, processed + 1):
    messages.extend(rpc(provider, "getrootnetmessages", dict(subnet_id=subnet, block_height=height), token))
for address, before in ((address1, before1), (address2, before2)):
    address = evm_address(address)
    deposits = [m for m in messages if m["kind"] == "fund" and m["msg"]["address"].lower() == address and m["msg"]["amount"] == int(sats)]
    assert len(deposits) == 1, f"expected one deposit message for {address}: {deposits}"
    message = deposits[0]
    tx = json.loads(subprocess.check_output(["bitcoin-cli", "getrawtransaction", message["txid"], "1"], text=True))
    block = json.loads(subprocess.check_output(["bitcoin-cli", "getblockheader", tx["blockhash"]], text=True))
    assert message["block_height"] == block["height"] and message["block_hash"] == tx["blockhash"], "deposit message must name its own Bitcoin block"
    # The reported head can lag the newest committed subnet block by one.
    for attempt in range(11):
        head = int(eth("eth_blockNumber", []), 16)
        if finality(head) >= block["height"]:
            break
        if attempt < 10:
            time.sleep(1)
    # Find the first subnet block that commits finality at or above that Bitcoin block.
    low, high = int(subnet_from, 16), head
    assert finality(low) < block["height"] <= finality(high), "deposit's finality transition is outside the observed range"
    while high - low > 1:
        middle = (low + high) // 2
        if finality(middle) >= block["height"]:
            high = middle
        else:
            low = middle
    assert int(eth("eth_getBalance", [address, hex(low)]), 16) == int(before, 16), "deposit credited before its Bitcoin block reached parent finality"
    assert int(eth("eth_getBalance", [address, hex(high)]), 16) == int(before, 16) + int(sats) * 10**10, "deposit not credited when its Bitcoin block reached parent finality"
    print(f"deposit to {address}: Bitcoin block {block['height']}, credited in subnet block {high} when parent finality reaches it")
PY
}

# Fault-only selections need the recorded checkpoint history exercised by recovery.
checkpoint_fixture_ready() { [ "$(cp_number)" -ge 2 ]; }
step_checkpoint_fixture() {
  wait_for "two checkpoints exist before injecting a fault" 0 checkpoint_fixture_ready
}

provider_stopped() { ! component_pid provider 4 >/dev/null; }
provider_restarted() {
  kill -0 "$1" 2>/dev/null || { tail -15 "$LOGS/provider-validator4.log" >&2; fail "validator4's provider exited on restart with its non-empty sign record"; }
  provider_ready 4
}
provider_height_reached() {
  provider_reply "$1" getconfirmedcount null | jq -e --argjson h "$2" '.error == null and .result >= $h' >/dev/null
}

step_provider_outage() {
  say "4b. provider4 stops; its node waits alive, the quorum advances, and provider4 restarts with its votes"
  local pid restarted cp_before from since
  wait_for "validators have connected to their earlier peers" 0 peers_connected
  require_check "validators 1-4 are seated for the outage" committee_is 1 2 3 4
  require_check "validators 1-3 hold enough power without validator4" quorum_without 4
  require_check "read-only sign-record reader starts" sign_record_reader_ok
  wait_for "provider4 has a non-empty persistent vote database" 0 sign_record_ready
  pid=$(component_pid provider 4) || fail "cannot identify exactly one provider4 process"
  cp_before=$(cp_number); from=$(comet_height 4); since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  track_node_logs 4 "$since" provider-outage
  node_processes 4 > "$LOGS/provider-outage-processes-before.txt"
  STOPPED_PROVIDER_ENV=/root/.ipc/validator4/.env
  kill -TERM "$pid"
  wait_for "provider4 process has stopped" 0 provider_stopped
  sign_record_snapshot "$LOGS/provider-outage-votes-before.json"
  if provider_ready 4 >/dev/null 2>&1; then fail "provider4 is still reachable during its outage"; fi
  observe_outage provider-outage "$cp_before"
  sign_record_snapshot "$LOGS/provider-outage-votes-stopped.json"
  check "provider4 cannot write votes while stopped" cmp -s \
    "$LOGS/provider-outage-votes-before.json" "$LOGS/provider-outage-votes-stopped.json"
  provider --env "$STOPPED_PROVIDER_ENV" >> "$LOGS/provider-validator4.log" 2>&1 &
  restarted=$!
  wait_for "provider4 restarted with its existing key and non-empty record" 0 provider_restarted "$restarted"
  STOPPED_PROVIDER_ENV=""
  check_recovered_node 4 1 "$from" provider-outage
  wait_for "provider4 preserves every old vote and signs new heights without duplicates" 0 sign_record_extended
  check "provider outage and recovery did not restart either L2 process" \
    node_processes_unchanged 4 "$LOGS/provider-outage-processes-before.txt"
  capture_node_logs 4 "$since" provider-outage
}

step_monitor_outage() {
  say "4c. monitor4 pauses; future-height checkpoint queries refuse stale state and the node waits"
  local frozen requested response cp_before from since number selector
  require_check "validators 1-4 are seated for the monitor outage" committee_is 1 2 3 4
  require_check "validators 1-3 hold enough power during the monitor outage" quorum_without 4
  cp_before=$(cp_number); from=$(comet_height 4); since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  track_node_logs 4 "$since" monitor-outage
  node_processes 4 > "$LOGS/monitor-outage-processes-before.txt"
  PAUSED_MONITOR_PID=$(component_pid monitor 4) || fail "cannot identify exactly one monitor4 process"
  kill -STOP "$PAUSED_MONITOR_PID"
  frozen=$(provider_reply 4 getconfirmedcount null | jq -er '.result | numbers')
  requested=$((frozen + 1))
  wait_block "$((frozen + 2))"
  number=$(cp_number)
  for selector in "" ",\"number\":$number"; do
    response=$(provider_reply 4 getsubnetcheckpoint \
      "{\"subnet_id\":\"$SUBNET\",\"max_block_height\":$requested$selector}")
    check "monitor4 at $frozen refuses requested Bitcoin height $requested with -32004 (selector '$selector')" \
      jq -e '.error.code == -32004 and (has("result") | not)' <<< "$response"
  done
  response=$(provider_reply 4 getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"max_block_height\":$frozen}")
  check "a processed historical height remains queryable while monitor4 is paused" \
    jq -e '.error == null and has("result")' <<< "$response"
  observe_outage monitor-outage "$cp_before"
  check "provider4 still answers while its monitor is paused" provider_ready 4
  check "monitor4 remained at its frozen height throughout the outage" \
    jq -e --argjson h "$frozen" '.result == $h' <<< "$(provider_reply 4 getconfirmedcount null)"
  kill -CONT "$PAUSED_MONITOR_PID"
  PAUSED_MONITOR_PID=""
  wait_for "monitor4 catches up through the previously refused height" 0 provider_height_reached 4 "$requested"
  response=$(provider_reply 4 getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"max_block_height\":$requested}")
  check "the same agreed-height request succeeds after monitor4 catches up" \
    jq -e '.error == null and has("result")' <<< "$response"
  check_recovered_node 4 1 "$from" monitor-outage
  check "monitor outage and recovery did not restart either L2 process" \
    node_processes_unchanged 4 "$LOGS/monitor-outage-processes-before.txt"
  capture_node_logs 4 "$since" monitor-outage
}

step_l2_replay() {
  say "4d. validator1 crashes and restarts with its block, application, WAL and EOTS data intact"
  local cp_before from since reference svc
  wait_for "the quorum has peer connections before the bootstrap node stops" 0 peers_connected
  require_check "validators 2-4 hold enough power without validator1" quorum_without 1
  cp_before=$(cp_number); from=$(comet_height 1); since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  track_node_logs 1 "$since" l2-replay
  reference=$(comet_height 2)
  STOPPED_L2_NODE=1
  # A zero grace period forces a crash without removing containers, volumes, WALs or sign records.
  docker stop --time 0 validator-1-cometbft validator-1-fendermint validator-1-ethapi >/dev/null
  wait_for "validators 2-4 continue blocks with validator1 offline" 0 node_at_height 2 "$((reference + CHECK_PERIOD))"
  wait_for "a new checkpoint commits blocks produced with validator1 offline" 0 checkpoint_past "$cp_before" "$reference"
  for svc in fendermint cometbft ethapi; do
    if container_running "validator-1-$svc"; then fail "validator-1-$svc restarted before the crash observation completed"; fi
  done
  docker start validator-1-fendermint >/dev/null
  docker start validator-1-cometbft validator-1-ethapi >/dev/null
  STOPPED_L2_NODE=""
  wait_for "validator1 resumes after the crash and replay" 0 node_at_height 1 "$((from + 1))"
  check_recovered_node 1 2 "$from" l2-replay
  capture_node_logs 1 "$since" l2-replay
  check "CometBFT logged its replay handshake after the crash" \
    grep -i 'ABCI Replay Blocks' "$LOGS/l2-replay-cometbft.log"
  echo "   replay diagnostics (execution and replay EOF failures fail the step):"
  grep -iE 'replay|appHeight|storeHeight|stateHeight' "$LOGS/l2-replay-cometbft.log" | tail -15
  CHECKS+=("ok: validator1 restarted and recovered matching blocks and state without error on replay / EOF")
}
checkpoint_past() {
  [ "$(cp_number)" -gt "$1" ] || return 1
  rpc getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\"}" \
    | jq -e --argjson h "$2" '.result.checkpoint_height > $h and .result.block_height > 0' >/dev/null
}

step_join_v5() {
  say "5. validator 5 joins with join-ue for $EPOCHS_V5 seating epochs"
  local cfg_before cp_before; cfg_before=$(config_number); cp_before=$(cp_number)
  join_ue 5 "$EPOCHS_V5"
  T5=$(timeout_of 5)
  [ -n "$T5" ] || fail "validator5's join did not carry a timeout"
  V5_REMOVAL=$((T5 - 3 * EPOCH_LENGTH))
  local h_zero; h_zero=$(getsubnet | jq -er '.result.h_0')
  check "validator5 timeout is a cutoff relative to H_0" [ "$(((T5 - h_zero) % EPOCH_LENGTH))" -eq 0 ]
  echo "   T5 = $T5 (exit from tip $T5)"
  touch "$STATE_DIR/v5_context.ready"
  wait_for "validator5's join recorded with its exact EOTS commitment" "$V5_REMOVAL" check_commitment_query 5
  wait_for "validator5's collateral made slashable on L1" "$V5_REMOVAL" v5_is_slashable
  wait_for "committee contains validators 1-5" "$V5_REMOVAL" committee_is 1 2 3 4 5
  local entry_epoch; entry_epoch=$(getsubnet | jq -er '.result.last_rotation_epoch')
  wait_rotation "$entry_epoch" "$cp_before" "$V5_REMOVAL"
  list_validators
  check "committee rotated (configuration $cfg_before -> $(config_number))" [ "$(config_number)" -gt "$cfg_before" ]
  check "committee contains validators 1-5" committee_is 1 2 3 4 5
  V5_CONFIG=$(config_number); V5_CP=$(cp_number)
}

step_traffic() {
  say "6. a withdrawal and a deposit while validator 5 is a member; unstake and unstake-ue refused"
  local to before2; to=$(new_btc_address); before2=$(l2_balance "$IPC_ADDRESS_OF_USER_2")
  require_check "validator5 still seated before traffic" committee_is 1 2 3 4 5
  require_check "validator5 collateral is not mature before the rejection checks" [ "$(tip)" -lt "$T5" ]
  cli user1 cross-msg release --from "$IPC_ADDRESS_OF_USER_1" --subnet "$SUBNET" btc --to "$to" "$WITHDRAWAL"
  cli user2 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_2" "$DEPOSIT"
  expect_fail "unstake of a UE validator refused" cli 5 subnet unstake --subnet "$SUBNET" btc --collateral 1000
  expect_fail "unstake-ue before maturity refused" cli 5 subnet unstake-ue --subnet "$SUBNET"
  wait_checkpoints 2
  wait_for "withdrawal of $WITHDRAWAL sats paid to $to" 0 received_is "$to" "$WITHDRAWAL"
  wait_deposit "$IPC_ADDRESS_OF_USER_2" "$before2" "$DEPOSIT"
}

step_removal() {
  say "7. validator 5's removal precedes maturity; check its later adoption checkpoint"
  local changes key h_zero removal_epoch; key=$(xonly 5)
  h_zero=$(getsubnet | jq -er '.result.h_0')
  removal_epoch=$(((V5_REMOVAL - h_zero) / EPOCH_LENGTH + 1))
  wait_block "$V5_REMOVAL"
  changes=$(rpc getstakechanges "{\"subnet_id\":\"$SUBNET\",\"block_height\":$((V5_REMOVAL - 1))}")
  check "no validator5 withdrawal one block before its removal cutoff" \
    jq -e --arg key "$key" '.result | all(.validator_xpk != $key or .change.withdraw == null)' <<< "$changes"
  changes=$(rpc getstakechanges "{\"subnet_id\":\"$SUBNET\",\"block_height\":$V5_REMOVAL}")
  check "exactly one validator5 withdrawal at its removal cutoff" \
    jq -e --arg key "$key" '[.result[] | select(.validator_xpk == $key and .change.withdraw != null)] | length == 1' <<< "$changes"
  check "validator5's full removal is recorded before collateral maturity" \
    jq -e --arg key "$key" --argjson timeout "$T5" --argjson amount "$COLLATERAL" --argjson cfg "$V5_CONFIG" --argjson removal "$V5_REMOVAL" \
      '.result | any(.validator_xpk == $key and .change.withdraw.amount == $amount
        and .block_height == $removal and .block_height < $timeout and .configuration_number > $cfg
        and ([.committee_after_change.validators[].pubkey] | index($key)) == null)' <<< "$changes"
  # Use the entry snapshot: a departure during traffic must not require another rotation.
  wait_rotation "$removal_epoch" "$V5_CP" 0
  wait_for "committee contains validators 1-4 after departure" 0 committee_is 1 2 3 4
  list_validators
  check "committee of 4 after the rotation" [ "$(committee_size)" -eq 4 ]
  local cp txid; cp=$(rotation_cp_after "$V5_CP" "$removal_epoch")
  [ -n "$cp" ] || fail "no departure checkpoint after checkpoint $V5_CP"
  txid=$(cp_txid "$cp")
  echo "   rotation checkpoint $txid outputs:"
  bitcoin-cli getrawtransaction "$txid" 1 | jq -c '.vout[] | {n, value, address: .scriptPubKey.address}'
  check "no unstake output for validator5 in the rotation checkpoint" \
    [ "$(bitcoin-cli getrawtransaction "$txid" 1 | jq -r --arg a "${BACKUP[5]}" '[.vout[] | select(.scriptPubKey.address == $a)] | length')" -eq 0 ]
  check "validator5's collateral has not paid out on departure" [ "$(received "${BACKUP[5]}")" -eq 0 ]
}

step_exit() {
  say "8. exit from tip T5 = $T5: unstake-ue pays the backup address"
  wait_block "$T5"
  local cutoff changes key; key=$(xonly 5)
  for cutoff in "$((V5_REMOVAL + EPOCH_LENGTH))" "$((V5_REMOVAL + 2 * EPOCH_LENGTH))" "$T5"; do
    changes=$(rpc getstakechanges "{\"subnet_id\":\"$SUBNET\",\"block_height\":$cutoff}")
    check "no repeated validator5 withdrawal at cutoff $cutoff" \
      jq -e --arg key "$key" '.result | all(.validator_xpk != $key or .change.withdraw == null)' <<< "$changes"
  done
  local out txid tx paid
  out=$(cli 5 subnet unstake-ue --subnet "$SUBNET") || { echo "$out"; fail "unstake-ue of validator5"; }
  echo "$out"
  check "unstake-ue stdout identifies the txid, collateral amount and backup address" \
    grep -Ex "unstake-ue transaction submitted: txid [0-9a-f]{64}; $COLLATERAL sats of collateral spent, paid minus the fee to ${BACKUP[5]}" <<< "$out"
  txid=$(sed -n 's/^unstake-ue transaction submitted: txid \([0-9a-f]\{64\}\);.*/\1/p' <<< "$out")
  wait_for "validator5's exit mined" 0 collateral_spent 5
  touch "$STATE_DIR/exit_spent.ready"
  check "collateral output of validator5 spent" collateral_spent 5
  tx=$(bitcoin-cli getrawtransaction "$txid" 1)
  check "exit spends the collateral through a timelocked input and pays only the backup address" \
    jq -e --arg commit "${COMMIT_TXID[5]}" --arg backup "${BACKUP[5]}" --argjson timeout "$T5" \
      '.version == 2 and .locktime == $timeout and (.vin | length == 1)
       and .vin[0].txid == $commit and .vin[0].vout == 1 and .vin[0].sequence == 4294967293
       and (.vout | length == 1) and .vout[0].scriptPubKey.address == $backup' <<< "$tx"
  paid=$(echo "$tx" | jq '(.vout[0].value * 100000000) | round')
  check "backup payout received exactly once" [ "$(received "${BACKUP[5]}")" -eq "$paid" ]
  check "backup payout is positive" [ "$paid" -gt 0 ]
  check "backup payout deducts a fee" [ "$paid" -lt "$COLLATERAL" ]
  echo "   backup address ${BACKUP[5]} received $paid sats, fee $((COLLATERAL - paid)) sats"
}

step_commitment_queries() {
  say "2d. all four joined EOTS identities and exact commitments are queryable before L2 startup"
  local n keys
  keys=$(rpc geteotskeys "{\"subnet_id\":\"$SUBNET\"}")
  check "four distinct EOTS keys for the four joiners" \
    jq -e '.result | length == 4 and ([.[].pk_eots] | unique | length) == 4' <<< "$keys"
  for n in 1 2 3 4; do
    check "validator$n commitment matches its join payload and L1 inclusion height" check_commitment_query "$n"
  done
}

eots_prefix_final() {
  local n port status
  for n in 1 2 3 4; do
    port=$((26657 + (n - 1) * 100))
    status=$(curl -fsS --max-time 5 --get --data-urlencode 'path="/eots"' \
      "http://${HOST_ADDR:-host.docker.internal}:$port/abci_query" \
      | jq -er '.result.response | select((.code // 0 | tonumber) == 0) | .value | @base64d | fromjson') || return 1
    printf '%s\n' "$status" > "$LOGS/eots-validator$n.json"
    jq -e --argjson height "$CHECK_PERIOD" \
      '.final_height >= $height and .recorded_final_height >= $height
       and .final_height >= .recorded_final_height' <<< "$status" >/dev/null || return 1
  done
}

step_eots_finality() {
  say "3b. all four validators finalize a subnet prefix through height $CHECK_PERIOD"
  wait_for "EOTS final and recorded heights reach $CHECK_PERIOD on validators 1-4" 0 eots_prefix_final
}

step_spent_commitment_queries() {
  say "8b. validator5's EOTS identity and commitment remain queryable after its collateral is spent"
  check "validator5 commitment remains recorded after exit" check_commitment_query 5
  local keys
  keys=$(rpc geteotskeys "{\"subnet_id\":\"$SUBNET\"}")
  check "all five accepted join identities remain recorded" jq -e '.result | length == 5' <<< "$keys"
}

step_rejoin() {
  say "9. validator 5 cannot join again after the rotation: a key joins a subnet once"
  local cfg_before; cfg_before=$(config_number)
  expect_fail "second join-ue of validator5 refused by the provider" cli 5 subnet join-ue --from "$(evm 5)" \
    --subnet "$SUBNET" --collateral "$COLLATERAL" --ip "${IP_PREFIX}4" --backup-address "${BACKUP[5]}" \
    --epochs "$EPOCHS_V5"
  wait_blocks 1
  check "committee still of 4" [ "$(committee_size)" -eq 4 ]
  check "no waiting committee" [ "$(waiting_size)" -eq 0 ]
  check "rejected rejoin changes no configuration" [ "$(config_number)" -eq "$cfg_before" ]
  check "rejected rejoin preserves validators 1-4" committee_is 1 2 3 4
}

step_steady() {
  say "10. steady state: a deposit and two more checkpoints"
  local before; before=$(l2_balance "$IPC_ADDRESS_OF_USER_1")
  cli user1 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_1" "$DEPOSIT"
  wait_checkpoints 2
  wait_deposit "$IPC_ADDRESS_OF_USER_1" "$before" "$DEPOSIT"
  check "validators 1-4 remain seated" committee_is 1 2 3 4
}

step_provider_key_isolation() {
  say "provider isolation: twelve independent validator EOTS keys and both sets of six RPC endpoints"
  local n subnet distinct f files=()
  for n in 1 2 3 4 5 6; do
    files+=("/root/.ipc/validator$n/eots.sk" "/root/.ipc/validator$n/eots-subnet-b.sk")
    for subnet in a b; do
      check "validator$n subnet $subnet provider answers on its assigned port" \
        jq -e '.error == null and (.result | type == "number")' \
        <<< "$(provider_reply "$n" getconfirmedcount null "$subnet")"
    done
  done
  for f in "${files[@]}"; do check "persistent EOTS key $f exists" test -s "$f"; done
  # Hashes establish distinctness without printing secret keys.
  distinct=$(sha256sum "${files[@]}" | awk '{print $1}' | sort -u | wc -l)
  check "all twelve EOTS key files are distinct" [ "$distinct" -eq 12 ]
}

step_bootstrap_a() {
  bootstrap_subnet_a_from_container.sh | tee "$LOGS/spec-bootstrap-a.log"
  PAIR_A=$(sed -n 's/^export SUBNET_ID="\([^"]*\)"$/\1/p' "$LOGS/spec-bootstrap-a.log" | tail -1)
  [ -n "$PAIR_A" ] || fail "subnet A bootstrap did not export its subnet id"
}

step_bootstrap_b() {
  bootstrap_subnet_b_from_container.sh | tee "$LOGS/spec-bootstrap-b.log"
  PAIR_B=$(sed -n 's/^export SUBNET_ID="\([^"]*\)"$/\1/p' "$LOGS/spec-bootstrap-b.log" | tail -1)
  [ -n "$PAIR_B" ] || fail "subnet B bootstrap did not export its subnet id"
}

step_bootstrap_pair() {
  say "bootstrap-pair: verify both helpers independently before either L2 starts"
  local subnet_a="$PAIR_A" subnet_b="$PAIR_B" keys_a keys_b
  check "the bootstrap helpers created distinct subnets" test "$subnet_a" != "$subnet_b"
  keys_a=$(provider_reply 1 geteotskeys "{\"subnet_id\":\"$subnet_a\"}" | jq -e 'select(.error == null) | .result')
  keys_b=$(provider_reply 1 geteotskeys "{\"subnet_id\":\"$subnet_b\"}" b | jq -e 'select(.error == null) | .result')
  check "A and B each have four joiners, the same validator keys, and eight different EOTS keys" \
    jq -ne --argjson a "$keys_a" --argjson b "$keys_b" \
      '($a | length == 4) and ($b | length == 4)
       and ([$a[].pubkey] | sort) == ([$b[].pubkey] | sort)
       and ([$a[].pk_eots, $b[].pk_eots] | unique | length == 8)
       and all($a[], $b[]; .pubkey != .pk_eots)'
}

subnet_b_active() { provider_reply 1 getsubnet "{\"subnet_id\":\"$PAIR_B\"}" b | jq -e '.result.h_0 != null' >/dev/null; }

step_subnet_b_cli_routing() {
  say "subnet B CLI routing: observe the request on 3130 and check a join through 3030 is refused"
  local log_a="$LOGS/provider-validator1.log" log_b="$LOGS/provider-validator1-subnet-b.log"
  local offset_a offset_b before after backup rc=0
  require_check "both provider logs exist for request attribution" test -f "$log_a"
  require_check "subnet B provider log exists for request attribution" test -f "$log_b"
  python3 - <<'PY'
import pathlib
import tomllib
for name, port in (("config.toml", 3030), ("subnet-b.config.toml", 3130)):
    config = tomllib.loads((pathlib.Path("/root/.ipc/validator1") / name).read_text())
    parent = [s for s in config["subnets"] if s["id"] == "/b4"]
    assert len(parent) == 1
    assert parent[0]["config"]["provider_http"] == f"http://127.0.0.1:{port}/api"
PY
  offset_a=$(stat -c %s "$log_a"); offset_b=$(stat -c %s "$log_b")
  timeout 30 ipc-cli --config-path /root/.ipc/validator1/subnet-b.config.toml \
    subnet list-validators --subnet "$PAIR_B" > "$LOGS/spec-subnet-b-cli.log" 2>&1
  tail -c "+$((offset_b + 1))" "$log_b" > "$LOGS/spec-subnet-b-cli-provider-b.log"
  tail -c "+$((offset_a + 1))" "$log_a" > "$LOGS/spec-subnet-b-cli-provider-a.log"
  check "ipc-cli's subnet B query reached provider 3130" \
    grep -F "getsubnet: $PAIR_B" "$LOGS/spec-subnet-b-cli-provider-b.log"
  if grep -F "getsubnet: $PAIR_B" "$LOGS/spec-subnet-b-cli-provider-a.log"; then
    fail "ipc-cli's subnet B query also reached provider 3030"
  fi
  # Subnet B must be active for the join to reach the key check.
  CHECKPOINT_TIMEOUT=$((3 * EPOCH_LENGTH)) wait_for "subnet B's monitor has set H_0" 0 subnet_b_active
  before=$(provider_reply 1 geteotskeys "{\"subnet_id\":\"$PAIR_B\"}" b | jq -ceS '.result | sort_by(.pubkey)')
  backup=$(new_btc_address)
  timeout 30 ipc-cli --config-path /root/.ipc/validator1/config.toml \
    subnet join-ue --from "$(evm 1)" --subnet "$PAIR_B" --collateral "$COLLATERAL" \
    --ip "${IP_PREFIX}0" --backup-address "$backup" --epochs "$EPOCHS_BOOTSTRAP" \
    > "$LOGS/spec-subnet-b-join-via-3030.log" 2>&1 || rc=$?
  check "provider 3030 refuses the subnet B join" test "$rc" -ne 0
  check "the refusal rejects a validator key that already joined subnet B" \
    grep -iE 'already joined|already registered in subnet|joins.*once|joined.*before|registered.*before|earlier.*join' "$LOGS/spec-subnet-b-join-via-3030.log"
  [ "$rc" -ne 124 ] && [ "$rc" -ne 137 ] || fail "the join timed out instead of being refused"
  after=$(provider_reply 1 geteotskeys "{\"subnet_id\":\"$PAIR_B\"}" b | jq -ceS '.result | sort_by(.pubkey)')
  check "the refused join changes no registered subnet B identities" test "$after" = "$before"
}

verify_bootstrap_subnet() {
  local subnet="$1" side="$2" keys n pubkey pk_eots commitments outputs expected
  keys=$(provider_reply 1 geteotskeys "{\"subnet_id\":\"$subnet\"}" "$side" | jq -e 'select(.error == null) | .result')
  SUBNET="$subnet"
  check "bootstrap $subnet seats validators 1-4" committee_is 1 2 3 4
  expected=$(for n in 1 2 3 4; do xonly "$n"; done | jq -Rsc 'split("\n") | map(select(length > 0)) | sort')
  outputs=$(rpc getunslashableoutputs "{\"subnet_id\":\"$subnet\"}")
  check "bootstrap $subnet records one UE collateral for every genesis validator" \
    jq -e --argjson expected "$expected" \
      '.result | length == 4 and ([.[].pubkey] | sort) == $expected' <<< "$outputs"
  for n in 1 2 3 4; do
    pubkey=$(xonly "$n")
    pk_eots=$(jq -er --arg pubkey "$pubkey" '.[] | select(.pubkey == $pubkey) | .pk_eots' <<< "$keys")
    commitments=$(rpc geteotscommitments "{\"subnet_id\":\"$subnet\",\"pk_eots\":\"$pk_eots\"}")
    check "bootstrap $subnet validator$n records a nonce range starting at genesis" \
      jq -e '.result | length == 1 and .[0].start == 1 and .[0].count > 0
        and (.[0].root | test("^[0-9a-f]{64}$")) and .[0].block_height > 0' <<< "$commitments"
  done
}
step_bootstrap_a_queries() { verify_bootstrap_subnet "$PAIR_A" a; }
step_bootstrap_b_queries() { verify_bootstrap_subnet "$PAIR_B" b; }

step_eots_nonce_seeds() {
  say "3c. inspect persistent nonce seeds"
  python3 - <<'PY'
import pathlib
seeds, signing_keys = [], []
for n in range(1, 7):
    env = {}
    for line in pathlib.Path(f"/root/.ipc/validator{n}/.env").read_text().splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            name, value = line.removeprefix("export ").split("=", 1)
            env[name.strip()] = value.strip().strip("\"'")
    assert env.get("EOTS_SEED_PATH"), f"validator{n}: EOTS_SEED_PATH missing"
    seed = bytes.fromhex(pathlib.Path(env["EOTS_SEED_PATH"]).read_text().strip())
    key = bytes.fromhex(pathlib.Path(env["EOTS_SK_PATH"]).read_text().strip())
    assert len(seed) == len(key) == 32 and seed != key
    seeds.append(seed)
    signing_keys.append(key)
assert len(set(seeds)) == 6 and not set(seeds).intersection(signing_keys)
print("six independent persisted nonce seeds, separate from the EOTS keys")
PY
}

# Search the recorded history: epoch 1's rotation need not be checkpoint number 0.
epoch_one_rotation() {
  local last n checkpoint
  last=$(getsubnet | jq -r '.result.last_checkpoint_number // empty') || return 1
  [ -n "$last" ] || return 1
  for ((n=0; n<=last; n++)); do
    checkpoint=$(rpc getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"number\":$n}") || return 1
    if jq -e '.result.epoch == 1 and .result.is_rotation_checkpoint' <<< "$checkpoint" >/dev/null; then
      jq -c '.result' <<< "$checkpoint"
      return 0
    fi
  done
  return 1
}
epoch_one_configuration_ready() {
  local state h_zero
  state=$(getsubnet) || return 1
  jq -e '.result.h_0 > 0 and .result.epoch >= 1' <<< "$state" >/dev/null || return 1
  h_zero=$(jq -r '.result.h_0' <<< "$state")
  rpc getstakechanges "{\"subnet_id\":\"$SUBNET\",\"block_height\":$h_zero}" \
    | jq -e --argjson keys "$1" --argjson amount "$COLLATERAL" --argjson unit "$((10 ** POWER_UNIT_EXPONENT))" \
      '.result | sort_by(.configuration_number) | last | .committee_after_change | objects
       | ([.validators[].pubkey] | sort) == $keys
         and all(.validators[]; .collateral == $amount and .power == ($amount / $unit | floor))' >/dev/null
}

# Checks only that epoch 1 starts.
step_epoch_checkpoint_timing() {
  say "3d. H_0 and C_1 are recorded; epoch 1's rotation reaches Bitcoin L1"
  local h_zero c_one keys rotation whitelist
  keys=$(for n in 1 2 3 4; do xonly "$n"; done | jq -Rsc 'split("\n") | map(select(length > 0)) | sort')
  wait_for "monitor set H_0 and computed C_1 from validators 1-4 and their seating collateral" 0 epoch_one_configuration_ready "$keys"
  h_zero=$(getsubnet | jq -er '.result.h_0')
  # Historical changes keep C_1 available even when this step runs again in a later epoch.
  c_one=$(rpc getstakechanges "{\"subnet_id\":\"$SUBNET\",\"block_height\":$h_zero}" \
    | jq -ce '.result | sort_by(.configuration_number) | last | .committee_after_change | objects')
  wait_for "epoch 1's rotation checkpoint recorded" 0 epoch_one_rotation
  rotation=$(epoch_one_rotation)
  whitelist=$(for n in 1 2 3 4 5 6; do xonly "$n"; done | jq -Rsc 'split("\n") | map(select(length > 0)) | sort')
  python3 - "$rotation" "$c_one" "$whitelist" "$MIN_VALIDATORS" "$SUBNET" <<'PY'
import base64
import hashlib
import json
import subprocess
import sys
rotation, c_one, whitelist = (json.loads(value) for value in sys.argv[1:4])
boot = int(sys.argv[4])
keys = [bytes.fromhex(k) for k in whitelist]
# C_wlist has unit powers and threshold floor(2*boot/3)+1.
threshold = 2 * boot // 3 + 1
whitelist_script = b"\x00" + b"".join(b"\x7c\x20" + k + b"\xac\x63\x51\x93\x68" for k in keys) + bytes([0x50 + threshold, 0xa2])
# The second leaf binds this multisig to this subnet, even when another subnet has the same whitelist.
encoded = sys.argv[5].rsplit("/", 1)[1][5:].upper()
subnet_id = base64.b32decode(encoded + "=" * (-len(encoded) % 8))[:20]
tag = hashlib.sha256(b"TapLeaf").digest()
domain_leaf = hashlib.sha256(tag + tag + b"\xc0\x16\x6a\x14" + subnet_id).digest()
internal_key = bytes.fromhex("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799")
def spends_whitelist(vin):
    script, control = (bytes.fromhex(item) for item in vin["txinwitness"][-2:])
    return (script == whitelist_script and len(control) == 65 and control[0] & 0xfe == 0xc0
            and control[1:] == internal_key + domain_leaf)

tx = json.loads(subprocess.check_output(["bitcoin-cli", "getrawtransaction", rotation["txid"], "1"], text=True))
assert tx.get("confirmations", 0) > 0, "epoch 1's rotation is not on Bitcoin L1"
assert tx["vin"] and all(spends_whitelist(v) for v in tx["vin"]), "epoch 1's rotation must spend this subnet's C_wlist fund UTXOs"
assert rotation["epoch"] == 1 and rotation["is_rotation_checkpoint"], "checkpoint is not epoch 1's flagged rotation"
assert tx["vout"][-1]["scriptPubKey"].get("address") == c_one["multisig_address"], "rotation change must pay MS(C_1)"
print(f"epoch 1 rotation {rotation['txid']} is mined, spends C_wlist funds and pays its change to MS(C_1)")
PY
}

# ---------------------------------------------------------------- run

# leftovers of a previous run on this devnet: the L2 containers (fixed names) and its relayers
clean_previous_run() {
  local c; c=$(docker ps -aq --filter 'name=^validator-[1-6]-(fendermint|cometbft|ethapi)$')
  [ -z "$c" ] || { echo "removing the L2 containers of a previous run"; docker rm -f $c >/dev/null; }
  pkill -f "checkpoint relayer" || true
}

# Read-only history audit, deliberately selected separately from `all`. The
# matrix calls it after removal, on the same saved fixture. No fixed P-multiple
# assumption is made about recorded checkpoint heights.
step_checkpoint_invariants() {
  say "checkpoint invariants for P=$CHECK_PERIOD subnet blocks, K=$EPOCH_LENGTH Bitcoin blocks"
  python3 - "$SUBNET" "$EPOCH_LENGTH" "$CHECK_PERIOD" "$VAL1_API_URL" "$VAL1_BEARER_TOKEN" \
    "${HOST_ADDR:-host.docker.internal}" "$GATEWAY" "$LOGS" "$(xonly 5)" "$T5" <<'PY'
import base64
import bisect
import collections
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import time
import urllib.request

subnet, k, period, provider, token, host, gateway, logdir, v5, t5 = sys.argv[1:]
k, period = int(k), int(period)
totals = collections.Counter({name: 0 for name in ("PASS", "FAIL", "OUT_OF_SCOPE", "DEFERRED")})
first_missing_rotation = None

def result(ok, epoch, location, description, category=None):
    if first_missing_rotation is not None and epoch >= first_missing_rotation:
        category = "OUT_OF_SCOPE"
        description += f"; rotation missing by cutoff in epoch {first_missing_rotation}"
    category = category or ("PASS" if ok else "FAIL")
    totals[category] += 1
    print(f"{category}\tepoch={epoch} {location}\t{description}".replace("\n", " "), flush=True)

def btc(*args):
    # bitcoin-cli prints a bare string (a block hash, a raw transaction) as plain text, not JSON
    out = subprocess.check_output(["bitcoin-cli", *map(str, args)], text=True).strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return out

def rpc(url, method, params, bearer=None):
    headers = {"Content-Type": "application/json"}
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    request = urllib.request.Request(url, json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=params)).encode(), headers)
    with urllib.request.urlopen(request, timeout=15) as response:
        reply = json.load(response)
    if reply.get("error") is not None:
        raise RuntimeError(f"{method}: {reply['error']}")
    return reply["result"]

def query(method, **params):
    return rpc(provider, method, dict(subnet_id=subnet, **params), token)

def eth(method, params):
    return rpc(f"http://{host}:8545/", method, params)

# Derive the unit-power whitelist/genesis script
# independently, including the subnet-binding leaf. Later configurations come
# from the cutoff's committee_after_change, never from a checkpoint's change.
field = 2**256 - 2**32 - 977
generator = (0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798,
             0x483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8)
def add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    x, y = a
    u, v = b
    if x == u and (y + v) % field == 0:
        return None
    slope = ((3*x*x) * pow(2*y, -1, field) if a == b else (v-y) * pow(u-x, -1, field)) % field
    nx = (slope*slope-x-u) % field
    return nx, (slope*(x-nx)-y) % field

def tagged(tag, data):
    h = hashlib.sha256(tag.encode()).digest()
    return hashlib.sha256(h + h + data).digest()

def compact(n):
    return bytes([n]) if n < 253 else b"\xfd" + n.to_bytes(2, "little")

def unit_multisig(keys, threshold, subnet_bytes):
    assert 1 <= threshold <= 16
    script = b"\x00" + b"".join(b"\x7c\x20" + bytes.fromhex(key) + b"\xac\x63\x51\x93\x68" for key in sorted(keys)) + bytes([0x50 + threshold, 0xa2])
    leaves = sorted(tagged("TapLeaf", b"\xc0" + compact(len(leaf)) + leaf) for leaf in (script, b"\x6a\x14" + subnet_bytes))
    internal_x = generator[0] + 1
    internal_y = pow((internal_x**3 + 7) % field, (field + 1)//4, field)
    internal = internal_x, internal_y if internal_y % 2 == 0 else field - internal_y
    tweak = int.from_bytes(tagged("TapTweak", internal_x.to_bytes(32, "big") + tagged("TapBranch", b"".join(leaves))), "big")
    point, multiple = None, generator
    while tweak:
        if tweak & 1:
            point = add(point, multiple)
        multiple = add(multiple, multiple)
        tweak >>= 1
    return "5120" + add(internal, point)[0].to_bytes(32, "big").hex()

def main():
    global first_missing_rotation
    genesis = query("getgenesisinfo")
    for attempt in range(10):
        processed = rpc(provider, "getconfirmedcount", None, token)
        state = query("getsubnet")
        if rpc(provider, "getconfirmedcount", None, token) == processed:
            break
    else:
        raise RuntimeError("could not capture a stable monitor height")
    params = genesis["create_subnet_msg"]
    h0 = state["h_0"]
    assert h0 is not None, "H_0 has not been recorded"
    # Freeze the monitor history. Later mining during this audit cannot add a
    # partial epoch or checkpoint to the observations below.
    current = (processed - h0)//k + 1
    result(params["epoch_length"] == k and params["bottomup_check_period"] == period,
           0, f"Bitcoin-height={h0}", "saved (P,K) agrees with the create transaction")
    checkpoints = []
    assert state["last_checkpoint_number"] is not None, "no checkpoint history"
    for number in range(state["last_checkpoint_number"] + 1):
        cp = query("getsubnetcheckpoint", number=number)
        assert cp is not None, f"checkpoint number {number} missing"
        if cp["block_height"] <= processed:
            checkpoints.append(cp)
    assert checkpoints, "no checkpoint recorded by the snapshot height"
    # Logs identify the two stated limits. Count attempts PER provider, never
    # by adding identical messages from different providers. The fixed-name
    # provider logs belong to subnet A; subnet B uses separate *-subnet-b logs.
    logs = pathlib.Path(logdir)
    missing = set()
    for path in logs.glob("monitor*.log"):
        for line in path.read_text(errors="replace").splitlines():
            match = re.search(r"Subnet " + re.escape(subnet) + r": no rotation checkpoint of epoch (\d+) by its cutoff block (\d+)", line)
            if match and 1 <= int(match[1]) < current and int(match[2]) <= processed:
                missing.add(int(match[1]))
    logged_missing = missing.copy()
    # A closed epoch without its rotation checkpoint in the recorded history is
    # OUT_OF_SCOPE in every cell, also when the monitor log has been rotated away.
    missing.update(epoch for epoch in range(1, current)
                   if not any(cp["is_rotation_checkpoint"] and cp["epoch"] == epoch
                              and cp["block_height"] <= h0 + epoch*k for cp in checkpoints))
    duplicate_epochs, attempted_epochs, attempted_heights = set(), collections.defaultdict(set), set()
    for path in sorted(logs.glob("provider-validator[1-6].log")):
        counts, height = collections.Counter(), None
        for line in path.read_text(errors="replace").splitlines():
            if "gen_checkpoint_psbt:" in line:
                match = re.search(r"checkpoint_height: (\d+)", line)
                height = int(match[1]) if match else None
                if height is not None:
                    attempted_heights.add(height)
            match = re.search(r"Rotation checkpoint of epoch (\d+)\b", line)
            if match:
                epoch = int(match[1])
                counts[epoch] += 1
                if height is not None:
                    attempted_epochs[height].add(epoch)
        duplicate_epochs.update(epoch for epoch, count in counts.items() if count >= 2)
    if missing:
        first_missing_rotation = min(missing)
        for epoch in sorted(missing):
            evidence = "monitor log and recorded history" if epoch in logged_missing else "recorded history (monitor log unavailable)"
            result(False, epoch, f"Bitcoin-height={h0 + epoch*k}", f"no rotation checkpoint by its next cutoff: {evidence}", "OUT_OF_SCOPE")

    encoded = subnet.rsplit("/", 1)[1][5:].upper()
    subnet_bytes = base64.b32decode(encoded + "=" * (-len(encoded) % 8))[:20]
    whitelist = unit_multisig(params["whitelist"], 2*params["min_validators"]//3 + 1, subnet_bytes)
    genesis_keys = {v["pubkey"] for v in genesis["genesis_validators"]}
    cgen = unit_multisig(genesis_keys, 2*len(genesis_keys)//3 + 1, subnet_bytes)
    scripts, numbers, members = {0: whitelist}, {0: 0}, {0: genesis_keys}
    changes_by_epoch = {}
    previous_script = cgen
    # A cutoff's transactions are applied before the configuration update becomes current.
    for epoch in range(1, current + 1):
        cutoff = h0 + (epoch-1)*k
        changes = sorted(query("getstakechanges", block_height=cutoff), key=lambda c: c["configuration_number"])
        changes_by_epoch[epoch] = changes
        previous_number = numbers[epoch-1]
        if changes:
            committee = changes[-1]["committee_after_change"]
            previous_script = btc("validateaddress", committee["multisig_address"])["scriptPubKey"]
            numbers[epoch] = committee["configuration_number"]
            members[epoch] = {v["pubkey"] for v in committee["validators"]}
        else:
            numbers[epoch], members[epoch] = previous_number, members[epoch-1]
        scripts[epoch] = previous_script
        expected = list(range(previous_number + 1, previous_number + len(changes) + 1))
        ok = ([c["configuration_number"] for c in changes] == expected
              and numbers[epoch] == previous_number + len(changes)
              and all(c["block_height"] == cutoff for c in changes))
        result(ok, epoch, f"Bitcoin-height={cutoff}", "invariant 6: change number advances exactly for cutoff updates and stays unchanged otherwise")

    # Read all Bitcoin transactions, not only monitor-accepted deposits. This
    # also reconstructs fund UTXOs at each PAST cutoff; a current scan alone
    # cannot detect a sweep that happened after its deadline.
    transactions, positions, spent, fund_outputs, block_times = {}, {}, {}, {}, []
    deposits = []
    known_scripts = set(scripts.values())
    for height in range(genesis["create_msg_block_height"], processed + 1):
        block = btc("getblock", btc("getblockhash", height), 2)
        block_times.append((height, block["time"]))
        for index, tx in enumerate(block["tx"]):
            txid = tx["txid"]
            transactions[txid], positions[txid] = tx, (height, index)
            for vin in tx["vin"]:
                if "txid" in vin:
                    spent[(vin["txid"], vin["vout"])] = ((height, index), txid)
            for output in tx["vout"]:
                if output["scriptPubKey"]["hex"] in known_scripts:
                    fund_outputs[(txid, output["n"])] = (output["scriptPubKey"]["hex"], (height, index))
            if not tx["vout"]:
                continue
            opreturn = bytes.fromhex(tx["vout"][0]["scriptPubKey"]["hex"])
            # Implemented deposit format: one 46-byte OP_RETURN push.
            if opreturn[:2] == b"\x6a\x2e" and opreturn[2:28] == b"IPCFND" + subnet_bytes:
                deposits.append(tx)

    # Check every monitor record against Bitcoin Core's transaction index too.
    # Keep the actual block time, rather than the time the monitor logged it.
    inclusion_times = {}
    for cp in checkpoints:
        tx = btc("getrawtransaction", cp["txid"], 1)
        header = btc("getblockheader", tx["blockhash"])
        assert tx.get("confirmations", 0) > 0, f"checkpoint-height={cp['checkpoint_height']} is not mined"
        assert header["height"] == positions[cp["txid"]][0]
        inclusion_times[cp["txid"]] = header["time"]

    by_epoch = collections.defaultdict(list)
    for cp in checkpoints:
        by_epoch[cp["epoch"]].append(cp)
    result(all(not cp["is_rotation_checkpoint"] for cp in by_epoch[0]), 0,
           f"Bitcoin-height={h0}", "invariant 1: epoch 0 checkpoints are regular")
    for epoch in range(1, current + 1):
        cps = by_epoch[epoch]
        rotations = [cp for cp in cps if cp["is_rotation_checkpoint"]]
        ok = (len(rotations) == 1 and bool(cps) and cps[0] is rotations[0]
              and all(not cp["is_rotation_checkpoint"] for cp in cps[1:]))
        if epoch == current and not cps:
            continue  # no exact-one assertion before the epoch's next cutoff
        result(ok, epoch, f"Bitcoin-height={h0 + epoch*k}", "invariant 1: first recorded checkpoint rotates, all others are regular")
        if epoch < current:
            result(len(rotations) == 1 and h0 + (epoch-1)*k < rotations[0]["block_height"] <= h0 + epoch*k,
                   epoch, f"Bitcoin-height={h0 + epoch*k}", "invariant 2: rotation included by H_e")

    # Obtain a successor even for the last recorded checkpoint. Periodic
    # creation heights plus BitcoinCheckpointAdded events include full-batch
    # checkpoints at arbitrary heights, including ones never recorded on L1.
    last_height = checkpoints[-1]["checkpoint_height"]
    next_period = (last_height//period + 1)*period
    deadline = time.monotonic() + 7*k
    while True:
        head = int(eth("eth_blockNumber", []), 16)
        if head >= next_period:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError(f"no successor subnet block {next_period} for checkpoint-height={last_height}")
        time.sleep(1)
    topic = subprocess.check_output(["cast", "keccak", "BitcoinCheckpointAdded(uint256,address,bytes32)"], text=True).strip()
    events = eth("eth_getLogs", [dict(address=gateway, fromBlock=hex(checkpoints[0]["checkpoint_height"]), toBlock=hex(head), topics=[topic])])
    created = {cp["checkpoint_height"] for cp in checkpoints}
    created.update(range((min(created)//period + 1)*period, head + 1, period))
    created.update(int(event["topics"][1], 16) for event in events)
    created.update(h for h in attempted_heights if min(created) <= h <= head)
    created = sorted(created)
    l2_blocks = {}
    def creation_time(height):
        if height not in l2_blocks:
            block = eth("eth_getBlockByNumber", [hex(height), False])
            assert block is not None, f"missing subnet block {height}"
            l2_blocks[height] = int(block["timestamp"], 16)
        return l2_blocks[height]
    unsafe = set()
    for cp in checkpoints:
        height, epoch = cp["checkpoint_height"], cp["epoch"]
        next_height = created[bisect.bisect_right(created, height)]
        included_time, next_time = inclusion_times[cp["txid"]], creation_time(next_height)
        if included_time >= next_time:
            unsafe.add(height)
        result(height not in unsafe, epoch, f"checkpoint-height={height}",
               f"Bitcoin-height={cp['block_height']} time={included_time}; next checkpoint-height={next_height} creation-time={next_time}; inclusion must precede creation",
               "OUT_OF_SCOPE" if height in unsafe else None)

    adopted_epoch = 0
    for index, cp in enumerate(checkpoints):
        height, epoch = cp["checkpoint_height"], cp["epoch"]
        tx = transactions[cp["txid"]]
        scope = "OUT_OF_SCOPE" if height in unsafe else None
        if cp["is_rotation_checkpoint"]:
            adopted_epoch = epoch
        result(epoch == adopted_epoch and bool(bytes.fromhex(tx["vout"][0]["scriptPubKey"]["hex"])[-1] & 4) == cp["is_rotation_checkpoint"],
               epoch, f"checkpoint-height={height}", "invariant 1: epoch follows the latest rotation and the recorded flag matches the transaction", scope)
        result(tx["vout"][-1]["scriptPubKey"]["hex"] == scripts[epoch], epoch,
               f"checkpoint-height={height}", "invariant 3: change pays MS(C_epoch)", scope)
        result(cp["next_configuration_number"] == numbers[epoch], epoch,
               f"checkpoint-height={height}", "invariant 6: recorded configuration change number matches its epoch (the applied wire change number may lag)", scope)
        result(cp["block_height"] == positions[cp["txid"]][0], epoch,
               f"checkpoint-height={height}", "recorded Bitcoin height matches its block", scope)
        if index:
            previous = checkpoints[index-1]
            previous_tx = transactions[previous["txid"]]
            # Judge only the presence of the previous change, not the
            # unimplemented ordering/selection of other fund inputs.
            chained = any(v.get("txid") == previous["txid"] and v.get("vout") == previous_tx["vout"][-1]["n"] for v in tx["vin"])
            chain_scope = "OUT_OF_SCOPE" if previous["checkpoint_height"] in unsafe else scope
            result(chained, epoch, f"checkpoint-height={height}",
                   f"invariant 5: spends change of checkpoint-height={previous['checkpoint_height']}", chain_scope)

    # Every periodic height between recorded checkpoints must be present;
    # full-batch/event heights are allowed too. Attribute a gap to its own
    # creation time or, preferentially, the provider's rotation-attempt epoch.
    recorded_heights = {cp["checkpoint_height"] for cp in checkpoints}
    for height in created:
        if not checkpoints[0]["checkpoint_height"] < height < last_height or height in recorded_heights:
            continue
        time_created = creation_time(height)
        # Block timestamps need not increase, so do not bisect them.
        bitcoin_height = max((h for h, stamp in block_times if stamp <= time_created), default=h0)
        epoch = 0 if bitcoin_height <= h0 else (bitcoin_height-h0-1)//k + 1
        attempts = attempted_epochs.get(height, set())
        epoch = min(attempts & duplicate_epochs or attempts or {epoch})
        result(False, epoch, f"checkpoint-height={height}", "invariant 5: created checkpoint missing from recorded history",
               "DEFERRED" if epoch in duplicate_epochs else None)

    # One current UTXO scan corroborates the historical reconstruction. Check
    # only outputs created before the rotation; delayed arrivals and the
    # unimplemented K/2 acceptance window are outside this audit.
    scan = btc("scantxoutset", "start", json.dumps([f"raw({script})" for script in sorted(known_scripts)]))
    assert scan["success"] and scan["height"] >= processed, "incomplete UTXO scan"
    unspent_now = {(u["txid"], u["vout"]) for u in scan["unspents"]}
    for epoch in range(1, current):
        if scripts[epoch] == scripts[epoch-1] and epoch != 1:
            continue
        rotation = next((cp for cp in by_epoch[epoch] if cp["is_rotation_checkpoint"]), None)
        cutoff = h0 + epoch*k
        if rotation is None:
            result(False, epoch, f"Bitcoin-height={cutoff}", "invariant 4: no rotation from which to judge the previous-multisig sweep")
            continue
        before = positions[rotation["txid"]]
        old_funds = [outpoint for outpoint, (script, position) in fund_outputs.items()
                     if script == scripts[epoch-1] and position < before
                     and (outpoint not in spent or spent[outpoint][0] >= before)]
        epoch_txids = {cp["txid"] for cp in by_epoch[epoch]}
        remaining = [outpoint for outpoint in old_funds
                     if outpoint not in spent or spent[outpoint][0][0] > cutoff
                     or spent[outpoint][1] not in epoch_txids or outpoint in unspent_now]
        result(not remaining, epoch, f"Bitcoin-height={cutoff}",
               f"invariant 4: previous multisig swept by next cutoff; remaining={remaining}")

    for tx in deposits:
        height = positions[tx["txid"]][0]
        epoch = 0 if height <= h0 else (height-h0-1)//k + 1
        location = f"deposit={tx['txid']}:1 Bitcoin-height={height}"
        result(tx["vout"][1]["scriptPubKey"]["hex"] == scripts[epoch], epoch, location,
               "invariant 7: deposit pays configuration current at its Bitcoin block")
        spend = spent.get((tx["txid"], 1))
        spender = next((cp for cp in checkpoints if spend and cp["txid"] == spend[1]), None)
        spend_epoch = spender["epoch"] if spender else current
        scope = "OUT_OF_SCOPE" if spender and spender["checkpoint_height"] in unsafe else None
        result(spender is not None and positions[spender["txid"]] > positions[tx["txid"]], spend_epoch, location,
               "invariant 7: a later recorded checkpoint spends the deposit", scope)
    result(bool(deposits), 0, f"Bitcoin-height={processed}", "invariant 7: observed deposits (including bootstrap funding)")
    if t5:
        entries = [e for e in range(1, current+1) if v5 in members[e] and v5 not in members[e-1]]
        departures = [e for e in range(1, current+1) if v5 not in members[e] and v5 in members[e-1]]
        for name, epochs in (("entry", entries), ("departure", departures)):
            epoch = epochs[0] if epochs else current
            result(bool(epochs) and all(changes_by_epoch[e] and numbers[e] > numbers[e-1] for e in epochs),
                   epoch, f"Bitcoin-height={h0 + (epoch-1)*k}", f"invariant 6: validator5 {name} increases the change number")

try:
    main()
except Exception as error:
    # Missing evidence is a failure, never a timing-limit classification.
    totals["FAIL"] += 1
    print(f"FAIL\tepoch=0 evidence\t{type(error).__name__}: {error}".replace("\n", " "), flush=True)
finally:
    for category, count in totals.items():
        print(f"{category}\tTOTAL\t{count}", flush=True)
sys.exit(1 if totals["FAIL"] else 0)
PY
}

ALL_STEPS="create bootstrap commitment_queries spin_up eots_finality eots_nonce_seeds epoch_checkpoint_timing deposits provider_outage monitor_outage join_v5 traffic removal exit spent_commitment_queries rejoin steady l2_replay"
FIXTURE_STEPS="checkpoint_fixture checkpoint_invariants"
PAIR_STEPS="provider_key_isolation bootstrap_a bootstrap_b bootstrap_a_queries bootstrap_b_queries bootstrap_pair"
# Needs subnet B active: run with --step after subnet B's monitor has set H_0.
PAIR_ACTIVE_STEPS="subnet_b_cli_routing"

usage() {
  cat <<'EOF'
Usage: ue_scenario.sh [bootstrap-pair | --group GROUP | --step STEP] [--state-dir DIR]
Groups: all (default), setup, deposits, lifecycle, steady, provider-outage, monitor-outage,
        replay, bootstrap-pair, pair-verify.
all runs every step once, in order, on one devnet: the cheapest full run. Another group on a
fresh devnet first runs the create, bootstrap and spin_up it needs.
Missing prerequisites run automatically. Completed prerequisites reuse saved state on the same
Bitcoin chain. The selected steps always run. Logs, checks and summary.tsv are under DIR.
A fresh devnet is required to repeat create, bootstrap, spin_up, join_v5, exit or a bootstrap helper.
Outages run after the deposits, before the lifecycle; the crash runs last. Each step restores
its interrupted services. Each step has its own timeout; UE_STEP_TIMEOUT=<seconds> sets all.
UE_BUILD_IMAGE=0 reuses an existing fendermint image.
UE_EPOCH_LENGTH=90 and UE_CHECK_PERIOD=30 select K (Bitcoin blocks) and P (subnet blocks).
Both persist in the scenario state; use a fresh state/devnet for a different (P,K).
The matrix runs --step checkpoint_invariants last; this step is excluded from group all.
It prints PASS, FAIL, OUT_OF_SCOPE and DEFERRED rows and totals, and fails only on FAIL.
subnet_b_cli_routing needs subnet B active: run it with --step on the bootstrap-pair state once
subnet B's monitor has set H_0.
EOF
}

# Seconds per step at 1 s blocks, about three times a passing run; the outage, replay and checkpoint
# steps wait for several checkpoints. Building the fendermint image adds three times its 411 s.
step_timeout() {
  local base overhead=0
  case "$1" in
    spin_up) base=720; if [ "${UE_BUILD_IMAGE:-1}" = 1 ]; then overhead=1200; fi ;;
    removal) base=900 ;;
    epoch_checkpoint_timing|provider_outage|monitor_outage|l2_replay) base=1440 ;;
    join_v5|exit) base=600 ;;
    deposits|checkpoint_fixture|traffic|steady) base=960 ;;
    eots_finality|subnet_b_cli_routing) base=360 ;;
    bootstrap|bootstrap_a|bootstrap_b) base=180 ;;
    checkpoint_invariants) echo $((12 * EPOCH_LENGTH + 900)); return ;;
    *) base=60 ;;
  esac
  echo $(( (base * EPOCH_LENGTH + 89) / 90 + overhead ))
}

dependencies() {
  case "$1" in
    bootstrap) echo create ;;
    commitment_queries|spin_up) echo bootstrap ;;
    eots_finality|eots_nonce_seeds|epoch_checkpoint_timing|deposits|steady|checkpoint_fixture|checkpoint_invariants) echo spin_up ;;
    join_v5) echo 'deposits_sent epoch_checkpoint_timing' ;;
    provider_outage|monitor_outage|l2_replay) echo checkpoint_fixture ;;
    traffic) echo 'join_v5 deposits_sent' ;;
    removal) echo join_v5 ;;
    exit|rejoin) echo v5_context ;;
    spent_commitment_queries) echo exit_spent ;;
    bootstrap_pair|subnet_b_cli_routing) echo 'bootstrap_a bootstrap_b' ;;
    bootstrap_a_queries) echo bootstrap_a ;;
    bootstrap_b_queries) echo bootstrap_b ;;
  esac
}

save_state() {
  declare -p BACKUP JOIN_TXID COMMIT_TXID T5 V5_REMOVAL V5_CONFIG V5_CP SUBNET PAIR_A PAIR_B EPOCH_LENGTH CHECK_PERIOD \
    | sed 's/^declare /declare -g /' > "$STATE_DIR/state.env.tmp"
  mv "$STATE_DIR/state.env.tmp" "$STATE_DIR/state.env"
}

finish_step() {
  local status=$? interrupted=0
  case "$status" in 130|143) interrupted="$status" ;; esac
  trap - EXIT ERR
  set +e
  if grep -q '^FAIL' "$STEP_CHECKS"; then status=1; fi
  if ! restore_outage_components "$status"; then
    printf 'FAIL\tinterrupted component restoration failed\n' >> "$STEP_CHECKS"
    status=1
  fi
  save_state || status=1
  if [ "$status" -eq 0 ]; then
    touch "$STATE_DIR/$internal.ready"
  else
    rm -f "$STATE_DIR/$internal.ready"
  fi
  printf '%s\n' "${CHECKS[@]}"
  [ "$interrupted" -eq 0 ] || exit "$interrupted"
  exit "$status"
}

group=all
selected=""
internal=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    bootstrap-pair) group=bootstrap-pair; shift ;;
    --group|--step|--state-dir|--internal-step)
      [ "$#" -ge 2 ] || fail "missing value for $1"
      case "$1" in
        --group) group="$2" ;;
        --step) selected="$2" ;;
        --state-dir) STATE_DIR="$2" ;;
        --internal-step) internal="$2" ;;
      esac
      shift 2 ;;
    *) usage >&2; fail "unknown argument $1" ;;
  esac
done

if [ -z "$selected$internal" ]; then
  case "$group" in
    all) selected="$ALL_STEPS" ;;
    setup) selected='create bootstrap commitment_queries spin_up eots_finality eots_nonce_seeds epoch_checkpoint_timing' ;;
    deposits) selected=deposits ;;
    lifecycle) selected='join_v5 traffic removal exit spent_commitment_queries rejoin steady' ;;
    steady) selected=steady ;;
    provider-outage) selected=provider_outage ;;
    monitor-outage) selected=monitor_outage ;;
    replay) selected=l2_replay ;;
    bootstrap-pair) selected="$PAIR_STEPS" ;;
    pair-verify) selected='provider_key_isolation bootstrap_a_queries bootstrap_b_queries bootstrap_pair' ;;
    *) fail "unknown group $group" ;;
  esac
fi
for id in ${internal:-$selected}; do
  case " $ALL_STEPS $PAIR_STEPS $PAIR_ACTIVE_STEPS $FIXTURE_STEPS " in *" $id "*) ;; *) fail "unknown step $id" ;; esac
done

# Preflight: a missing prerequisite of a selected step stops the run before any step mutates the devnet.
if [ -z "$internal" ]; then
  case " $selected " in
    *" provider_outage "*) sign_record_reader_ok || fail "preflight: provider_outage needs the sign-record reader" ;;
  esac
fi

mkdir -p "$STATE_DIR" "$LOGS"
STATE_DIR=$(cd "$STATE_DIR" && pwd)
if [ -f "$STATE_DIR/state.env" ]; then source "$STATE_DIR/state.env"; fi
[[ "$EPOCH_LENGTH" =~ ^[1-9][0-9]*$ && "$CHECK_PERIOD" =~ ^[1-9][0-9]*$ ]] \
  || fail "UE_EPOCH_LENGTH and UE_CHECK_PERIOD must be positive integers"
[ "$((3 * EPOCH_LENGTH))" -le 65535 ] || fail "UE_EPOCH_LENGTH must satisfy 3K <= 65535"
[ "${UE_EPOCH_LENGTH:-$EPOCH_LENGTH}" = "$EPOCH_LENGTH" ] \
  && [ "${UE_CHECK_PERIOD:-$CHECK_PERIOD}" = "$CHECK_PERIOD" ] \
  || fail "requested (P,K) differs from saved state; use a fresh devnet/state directory"
CHECKPOINT_TIMEOUT=$((7 * EPOCH_LENGTH))

if [ -n "$internal" ]; then
  STEP_CHECKS="$STATE_DIR/$internal.checks"
  : > "$STEP_CHECKS"
  trap finish_step EXIT
  trap 'printf "FAIL\tline %s: %s (rc=%s)\n" "$LINENO" "$BASH_COMMAND" "$?" >> "$STEP_CHECKS"' ERR
  trap 'exit 130' INT
  trap 'exit 143' TERM
  "step_$internal"
  exit 0
fi

# A block hash binds saved state to the exact regtest chain, including across container restarts.
if [ -f "$STATE_DIR/chain" ]; then
  read -r anchor_height anchor_hash < "$STATE_DIR/chain"
  [ "$(bitcoin-cli getblockhash "$anchor_height")" = "$anchor_hash" ] \
    || fail "saved fixture belongs to another chain; use a new --state-dir"
else
  anchor_height=$(tip)
  printf '%s %s\n' "$anchor_height" "$(bitcoin-cli getblockhash "$anchor_height")" > "$STATE_DIR/chain"
fi

# Clean fixed-name L2 containers only for a new fixture, never for a saved-state rerun.
if [ ! -f "$STATE_DIR/state.env" ]; then clean_previous_run; fi
: > "$STATE_DIR/summary.tsv"
declare -A ATTEMPTED
round_failed=0

run_step() {
  local id="$1" force="${2:-0}" dep producer rc started
  if [ -n "${ATTEMPTED[$id]:-}" ]; then return 0; fi
  if [ "$force" = 0 ] && [ -f "$STATE_DIR/$id.ready" ]; then return 0; fi
  ATTEMPTED[$id]=1
  for dep in $(dependencies "$id"); do
    producer="$dep"
    case "$dep" in v5_context) producer=join_v5 ;; exit_spent) producer=exit ;; deposits_sent) producer=deposits ;; esac
    if [ ! -f "$STATE_DIR/$dep.ready" ]; then run_step "$producer"; fi
    if [ ! -f "$STATE_DIR/$dep.ready" ]; then
      printf 'SKIP\t%s\tprerequisite %s unavailable\n' "$id" "$dep" | tee -a "$STATE_DIR/summary.tsv"
      round_failed=1
      return 0
    fi
  done
  case "$id" in
    create|bootstrap|spin_up|join_v5|exit|bootstrap_a|bootstrap_b)
      if [ -f "$STATE_DIR/$id.started" ]; then
        printf 'FAIL\t%s\tone-use mutation requires a fresh devnet/state directory\n' "$id" | tee -a "$STATE_DIR/summary.tsv"
        round_failed=1
        return 0
      fi ;;
  esac
  rm -f "$STATE_DIR/$id.ready"
  touch "$STATE_DIR/$id.started"
  started=$SECONDS
  # The child is a simple command, outside an if/&&/|| context: Bash errexit stays active.
  set +e
  timeout --signal=TERM --kill-after=90s "${UE_STEP_TIMEOUT:-$(step_timeout "$id")}s" \
    bash "${BASH_SOURCE[0]}" --internal-step "$id" --state-dir "$STATE_DIR" 2>&1 | tee "$STATE_DIR/$id.log"
  rc=${PIPESTATUS[0]}
  set -e
  if [ "$rc" -eq 0 ]; then
    printf 'PASS\t%s\t%ss\n' "$id" "$((SECONDS - started))" | tee -a "$STATE_DIR/summary.tsv"
  else
    printf 'FAIL\t%s\trc=%s, %ss\n' "$id" "$rc" "$((SECONDS - started))" | tee -a "$STATE_DIR/summary.tsv"
    round_failed=1
    [ "$rc" -ne 130 ] && [ "$rc" -ne 143 ] || exit "$rc"
  fi
}

for id in $selected; do run_step "$id" 1; done
say "results: $STATE_DIR/summary.tsv"
cat "$STATE_DIR/summary.tsv"
for id in ${!ATTEMPTED[@]}; do
  [ ! -f "$STATE_DIR/$id.checks" ] || grep '^FAIL' "$STATE_DIR/$id.checks" || true
done
exit "$round_failed"
