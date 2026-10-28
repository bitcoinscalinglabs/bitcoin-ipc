#!/bin/bash
set -euo pipefail

# Unilateral-exit scenario on the local devnet. Run INSIDE the bitcoin-ipc container, on a fresh
# devnet started with UE_ACTIVATION_HEIGHT=1:
#
#   make local-delete-data && make local-build && UE_ACTIVATION_HEIGHT=1 make local-up
#   docker exec -it bitcoin-ipc ue_scenario.sh
#
# Every protocol command goes through ipc-cli; the miner produces one block per 20 s and
# bitcoin-cli reads the chain. Each step prints the commands it runs and the checks it makes,
# and the script stops at the first failed check. Steps (functions at the bottom):
#   1 create       the subnet, from validator1, with validators 1-6 whitelisted
#   2 bootstrap    validators 1-4 join with join-ue (lock LOCK_BOOTSTRAP); lock-blocks 2 refused;
#                  the L2 accounts of validators 1-5 are funded (a validator signs from its L2 account)
#   3 spin_up      fendermint, CometBFT and the relayers for validators 1-4
#   4 deposits     users 1 and 2 deposit; two checkpoints by the UE-only committee
#   5 join_v5      validator 5 joins with join-ue (lock LOCK_V5) and is rotated in
#   6 traffic      a withdrawal and a deposit go through a checkpoint; unstake and unstake-ue refused
#   7 removal      at block T5 - 1 the monitor removes validator 5; the next checkpoint rotates it out
#   8 exit         at tip T5 unstake-ue pays the backup address
#   9 rejoin       validator 5 joins again and is rotated in
#  10 steady       two more checkpoints and a deposit
# Durations: about 30 minutes, dominated by LOCK_V5.

LOCK_BOOTSTRAP=300    # blocks after the tip: validators 1-4, longer than the run
LOCK_V5=30            # blocks after the tip: validator 5, about 10 minutes
COLLATERAL=100000000  # sats, the minimum validator stake below
MIN_VALIDATOR_STAKE=100000000
MIN_VALIDATORS=4
CHECK_PERIOD=30       # L2 blocks per checkpoint
DEPOSIT=50000000      # sats per deposit
WITHDRAWAL=10000000   # sats released from the L2 while validator 5 is a member
VALIDATOR_FUND=100000000 # sats deposited to each validator's L2 account: it must exist before the validator
                         # signs, and fendermint's gas pre-validation wants a balance of about 0.2 per signature tx
CHECKPOINT_TIMEOUT=600   # seconds without a new checkpoint before the run fails
IP_PREFIX="66.222.44.55:808"   # validator n advertises ${IP_PREFIX}$((n - 1))
SPIN_UP_V5=${SPIN_UP_V5:-1}    # also start validator 5's L2 node once it is in the committee

PARENT="/b4"
LOGS=/root/.ipc/logs
VAL1_API_URL="http://127.0.0.1:3030/api"
VAL1_BEARER_TOKEN="validator1_auth_token"
export RUST_LOG=${RUST_LOG_CLI:-info}   # ipc-cli logs the txids at info

declare -A BACKUP JOIN_TXID COMMIT_TXID
T5=""
SUBNET=""
CHECKS=()

# ---------------------------------------------------------------- helpers

say() { echo; echo "### $*"; }

fail() { echo "FAILED: $*" >&2; exit 1; }

# check <description> <command...>: the command's exit status is the check
check() {
  local what="$1"; shift
  if "$@"; then echo "ok: $what"; CHECKS+=("ok: $what"); else fail "$what"; fi
}

# expect_fail <description> <command...>: the command must fail; its last error line is printed
expect_fail() {
  local what="$1"; shift
  local out
  if out=$("$@" 2>&1); then echo "$out"; fail "$what (the command succeeded)"; fi
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
  resp=$(curl -sS --fail-with-body -X POST "$VAL1_API_URL" \
    -H "Content-Type: application/json" -H "Authorization: Bearer $VAL1_BEARER_TOKEN" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}")
  if echo "$resp" | jq -e '.error != null' >/dev/null 2>&1; then
    echo "$resp" | jq >&2; fail "provider $1"
  fi
  printf '%s' "$resp"
}

getsubnet() { rpc getsubnet "{\"subnet_id\":\"$SUBNET\"}"; }
committee_size() { getsubnet | jq '.result.committee.validators | length'; }
waiting_size() { getsubnet | jq '.result.waiting_committee.validators // [] | length'; }
config_number() { getsubnet | jq '.result.committee.configuration_number'; }
cp_number() { getsubnet | jq '.result.last_checkpoint_number // 0'; }
cp_txid() { rpc getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"number\":$1}" | jq -r '.result.txid'; }

tip() { bitcoin-cli getblockcount; }
wait_block() { echo "   waiting for block $1 (tip $(tip))"; while [ "$(tip)" -lt "$1" ]; do sleep 5; done; }
wait_blocks() { wait_block $(( $(tip) + $1 )); }
# wait_checkpoints <k>: until k more checkpoints are recorded by the monitor
wait_checkpoints() {
  local target=$(( $(cp_number) + $1 )) waited=0
  echo "   waiting for checkpoint number $target (now $(cp_number), about one per minute)"
  while [ "$(cp_number)" -lt "$target" ]; do
    sleep 10; waited=$((waited + 10))
    [ "$waited" -lt "$CHECKPOINT_TIMEOUT" ] || fail "no checkpoint $target after $CHECKPOINT_TIMEOUT s"
  done
  echo "   checkpoint $target recorded at tip $(tip)"
}
# wait_rotation <cfg_before>: until the committee's configuration number exceeds cfg_before (a
# checkpoint already in flight when a join or removal lands does not carry the rotation)
wait_rotation() {
  local waited=0
  echo "   waiting for a rotation past configuration $1 (now checkpoint $(cp_number), tip $(tip))"
  while [ "$(config_number)" -le "$1" ]; do
    sleep 5; waited=$((waited + 5))
    [ "$waited" -lt "$CHECKPOINT_TIMEOUT" ] || fail "no rotation past configuration $1 after $CHECKPOINT_TIMEOUT s"
  done
  echo "   configuration $(config_number) after checkpoint $(cp_number), tip $(tip)"
}
# rotation_cp_after <cp_before>: the first checkpoint after cp_before that rotates the committee
# (its signing committee differs from the next one; ordinary checkpoints repeat the current numbers)
rotation_cp_after() {
  local n
  for n in $(seq $(( $1 + 1 )) "$(cp_number)"); do
    if rpc getsubnetcheckpoint "{\"subnet_id\":\"$SUBNET\",\"number\":$n}" \
        | jq -e '.result.signed_committee_number != .result.next_committee_number' >/dev/null; then
      echo "$n"; return
    fi
  done
}

evm() { local v="IPC_ADDRESS_OF_VALIDATOR_$1"; echo "${!v}"; }
# x-only key of validator n: the EVM keystore key, uncompressed "04 X Y" from ipc-cli, X = 64 hex chars
xonly() { cli "$1" wallet pub-key --wallet-type evm --address "$(evm "$1")" | tail -1 | cut -c3-66; }
new_btc_address() { bitcoin-cli -rpcwallet=default getnewaddress; }
received() { bitcoin-cli -rpcwallet=default getreceivedbyaddress "$1" 1 | awk '{printf "%.0f", $1 * 100000000}'; }
list_validators() { cli 1 subnet list-validators --subnet "$SUBNET"; }

# join_ue <n> <lock_blocks>: validator n joins with join-ue; records its backup address and txids
join_ue() {
  local n="$1" lock="$2" out
  BACKUP[$n]=$(new_btc_address)
  if ! out=$(cli "$n" subnet join-ue --from "$(evm "$n")" --subnet "$SUBNET" --collateral "$COLLATERAL" \
    --ip "${IP_PREFIX}$((n - 1))" --backup-address "${BACKUP[$n]}" --lock-blocks "$lock" 2>&1); then
    echo "$out"; fail "join-ue of validator$n"
  fi
  echo "$out"
  JOIN_TXID[$n]=$(echo "$out" | grep -o 'Joined subnet with txid: [0-9a-f]*' | awk '{print $NF}')
  [ -n "${JOIN_TXID[$n]}" ] || fail "join-ue of validator$n: no txid in the output"
  COMMIT_TXID[$n]=$(bitcoin-cli getrawtransaction "${JOIN_TXID[$n]}" 1 | jq -r '.vin[0].txid')
  echo "   validator$n: join txid ${JOIN_TXID[$n]}, collateral output ${COMMIT_TXID[$n]}:1, backup ${BACKUP[$n]}"
}

# collateral_unspent <n>: the collateral output of validator n's last join-ue is unspent on chain
collateral_unspent() { [ "$(bitcoin-cli gettxout "${COMMIT_TXID[$1]}" 1 false | jq -r '.value // empty')" != "" ]; }
collateral_spent() { ! collateral_unspent "$1"; }

# timeout_of <n>: T of validator n's last join-ue, from its provider's log
timeout_of() { grep "join-ue): timeout" "$LOGS/provider-validator$1.log" | tail -1 | sed -n 's/.*timeout \([0-9]*\) = .*/\1/p'; }

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
    --validator-whitelist "$whitelist" 2>&1)
  echo "$out"
  SUBNET=$(echo "$out" | grep -o 'created subnet with id: [^ ]*' | awk '{print $NF}')
  [ -n "$SUBNET" ] || fail "create: no subnet id in the output"
  echo "SUBNET=$SUBNET"
  wait_blocks 1
  # the subnet state exists only once bootstrapped; before that the monitor holds the genesis info
  check "subnet $SUBNET known to the monitor" rpc getgenesisinfo "{\"subnet_id\":\"$SUBNET\"}"
}

step_bootstrap() {
  say "2. bootstrap: validators 1-4 join with join-ue, lock $LOCK_BOOTSTRAP blocks"
  for n in 1 2 3 4; do join_ue "$n" "$LOCK_BOOTSTRAP"; done
  say "2b. a lock of 2 blocks is refused (the monitor needs T > reveal block + 1)"
  expect_fail "join-ue with --lock-blocks 2 refused" cli 5 subnet join-ue --from "$(evm 5)" --subnet "$SUBNET" \
    --collateral "$COLLATERAL" --ip "${IP_PREFIX}4" --backup-address "$(new_btc_address)" --lock-blocks 2
  wait_blocks 2
  list_validators
  check "committee of 4" [ "$(committee_size)" -eq 4 ]
  for n in 1 2 3 4; do check "collateral output of validator$n unspent on chain" collateral_unspent "$n"; done
  add_subnet_to_configs
  say "2c. fund the L2 accounts of validators 1-5 (fendermint signs checkpoints from the validator's L2 account)"
  for n in 1 2 3 4 5; do cli user1 cross-msg fund --subnet "$SUBNET" btc --to "$(evm "$n")" "$VALIDATOR_FUND"; done
  wait_blocks 1
}

step_spin_up() {
  say "3. spin up the L2 (fendermint, CometBFT, relayers of validators 1-4)"
  echo "+ spin_up_subnet_a_from_container.sh $SUBNET"
  spin_up_subnet_a_from_container.sh "$SUBNET" | tee "$LOGS/spin-up-subnet-a.log"
  # the validator5 mode of the spin-up script needs the two ids it printed
  export CometBftID; CometBftID=$(grep -m1 '^export CometBftID=' "$LOGS/spin-up-subnet-a.log" | cut -d= -f2)
  export ResolverAddress; ResolverAddress=$(grep -m1 '^export ResolverAddress=' "$LOGS/spin-up-subnet-a.log" | cut -d= -f2)
  [ -n "$CometBftID" ] && [ -n "$ResolverAddress" ] || fail "the spin-up did not print CometBftID and ResolverAddress"
}

step_deposits() {
  say "4. deposits by users 1 and 2, then two checkpoints by the UE-only committee"
  cli user1 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_1" "$DEPOSIT"
  cli user2 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_2" "$DEPOSIT"
  wait_blocks 1
  wait_checkpoints 2
  cli 1 checkpoint last-bottomup-checkpoint-height --subnet "$SUBNET"
  check "two checkpoints recorded" [ "$(cp_number)" -ge 2 ]
}

step_join_v5() {
  say "5. validator 5 joins with join-ue, lock $LOCK_V5 blocks"
  local cfg_before; cfg_before=$(config_number)
  join_ue 5 "$LOCK_V5"
  wait_blocks 1
  T5=$(timeout_of 5)
  [ -n "$T5" ] || fail "validator5's provider did not log the timeout"
  echo "   T5 = $T5 (removal at block $((T5 - 1)), exit from tip $T5)"
  check "validator5 waiting for the next checkpoint" [ "$(waiting_size)" -eq 5 ]
  wait_rotation "$cfg_before"
  list_validators
  check "committee rotated (configuration $cfg_before -> $(config_number))" [ "$(config_number)" -gt "$cfg_before" ]
  check "committee of 5" [ "$(committee_size)" -eq 5 ]
  if [ "$SPIN_UP_V5" = "1" ]; then
    echo "+ spin_up_subnet_a_from_container.sh $SUBNET validator5"
    spin_up_subnet_a_from_container.sh "$SUBNET" validator5
  fi
}

step_traffic() {
  say "6. a withdrawal and a deposit while validator 5 is a member; unstake and unstake-ue refused"
  local to; to=$(new_btc_address)
  cli user1 cross-msg release --from "$IPC_ADDRESS_OF_USER_1" --subnet "$SUBNET" btc --to "$to" "$WITHDRAWAL"
  cli user2 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_2" "$DEPOSIT"
  expect_fail "unstake of a UE validator refused" cli 5 subnet unstake --subnet "$SUBNET" btc --collateral 1000
  expect_fail "unstake-ue before maturity refused" cli 5 subnet unstake-ue --subnet "$SUBNET"
  wait_checkpoints 2
  wait_blocks 1
  check "withdrawal of $WITHDRAWAL sats paid to $to" [ "$(received "$to")" -eq "$WITHDRAWAL" ]
}

step_removal() {
  say "7. removal at block T5 - 1 = $((T5 - 1)), then the next checkpoint rotates validator 5 out"
  local cp_before cfg_before; cp_before=$(cp_number); cfg_before=$(config_number)
  wait_block $((T5 - 1)); sleep 10
  grep "UE validator $(xonly 5) matured" "$LOGS/monitor-validator1.log" | tail -1
  check "monitor removed validator5 from the next committee" \
    grep -q "UE validator $(xonly 5) matured at block $((T5 - 1)): removed" "$LOGS/monitor-validator1.log"
  check "waiting committee of 4" [ "$(waiting_size)" -eq 4 ]
  wait_rotation "$cfg_before"
  list_validators
  check "committee of 4 after the rotation" [ "$(committee_size)" -eq 4 ]
  local cp txid; cp=$(rotation_cp_after "$cp_before")
  [ -n "$cp" ] || fail "no rotation checkpoint after checkpoint $cp_before"
  txid=$(cp_txid "$cp")
  echo "   rotation checkpoint $txid outputs:"
  bitcoin-cli getrawtransaction "$txid" 1 | jq -c '.vout[] | {n, value, address: .scriptPubKey.address}'
  check "no unstake output for validator5 in the rotation checkpoint" \
    [ "$(bitcoin-cli getrawtransaction "$txid" 1 | jq -r --arg a "${BACKUP[5]}" '[.vout[] | select(.scriptPubKey.address == $a)] | length')" -eq 0 ]
}

step_exit() {
  say "8. exit at tip T5 = $T5: unstake-ue pays the backup address"
  wait_block "$T5"
  cli 5 subnet unstake-ue --subnet "$SUBNET"
  wait_blocks 1
  check "collateral output of validator5 spent" collateral_spent 5
  echo "   backup address ${BACKUP[5]} received $(received "${BACKUP[5]}") sats (collateral $COLLATERAL minus the fee)"
  check "backup address paid" [ "$(received "${BACKUP[5]}")" -gt $(( COLLATERAL - 100000 )) ]
}

step_rejoin() {
  say "9. validator 5 joins again after the rotation"
  local cfg_before; cfg_before=$(config_number)
  join_ue 5 "$LOCK_V5"
  wait_blocks 1
  wait_rotation "$cfg_before"
  list_validators
  check "committee of 5 again" [ "$(committee_size)" -eq 5 ]
}

step_steady() {
  say "10. steady state: a deposit and two more checkpoints"
  cli user1 cross-msg fund --subnet "$SUBNET" btc --to "$IPC_ADDRESS_OF_USER_1" "$DEPOSIT"
  wait_checkpoints 2
  check "checkpoints keep coming" true
}

# ---------------------------------------------------------------- run

# leftovers of a previous run on this devnet: the L2 containers (fixed names) and its relayers
clean_previous_run() {
  local c; c=$(docker ps -aq --filter 'name=^validator-[1-6]-(fendermint|cometbft|ethapi)$')
  [ -z "$c" ] || { echo "removing the L2 containers of a previous run"; docker rm -f $c >/dev/null; }
  pkill -f "checkpoint relayer" || true
}

clean_previous_run
step_create
step_bootstrap
step_spin_up
step_deposits
step_join_v5
step_traffic
step_removal
step_exit
step_rejoin
step_steady

say "done. SUBNET=$SUBNET T5=$T5"
printf '%s\n' "${CHECKS[@]}"
