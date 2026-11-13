# Spin up subnet A

### 1. Shell in the container

```bash
docker exec -it bitcoin-ipc bash
```

```bash
export SUBNET_ID=<subnet-id>
```

```bash
cd /workspace/ipc
```

Build the fendermint image if missing:

```bash
docker image inspect fendermint:latest > /dev/null 2>&1 || (cd fendermint && make docker-build)
```

Empty CometBFT peer list:

```bash
COMETBFT_PEERS=""
```

Empty resolver peer list:

```bash
RESOLVER_PEERS=""
```

Helper: start one validator:

```bash
start_validator() {
  cargo-make make --makefile infra/fendermint/Makefile.toml \
    --env NODE_NAME="validator-$1" \
    --env SUBNET_ID="$SUBNET_ID" \
    --env HOME="$HOST_HOME_DIR" \
    --env PRIVATE_KEY_PATH="$HOME/.ipc/validator$1/validator.sk" \
    --env CMT_P2P_HOST_PORT="$2" \
    --env CMT_RPC_HOST_PORT="$3" \
    --env ETHAPI_HOST_PORT="$4" \
    --env RESOLVER_HOST_PORT="$5" \
    --env PERSISTENT_PEERS="$COMETBFT_PEERS" \
    --env RESOLVER_BOOTSTRAPS="$RESOLVER_PEERS" \
    --env PARENT_ENDPOINT="http://$HOST_ADDR:$6/api" \
    --env PARENT_AUTH_TOKEN="validator$1_auth_token" \
    --env TOPDOWN_CHAIN_HEAD_DELAY=0 \
    --env TOPDOWN_PROPOSAL_DELAY=0 \
    --env FM_PULL_SKIP=1 \
    --env FM_LOG_LEVEL="info,fendermint=info,tower=warn,libp2p=warn,tendermint=warn" \
    --env ETHAPI_LOG_LEVEL="info,fendermint=info,tower=warn,libp2p=warn,tendermint=warn" \
    child-validator > "$HOME/.ipc/logs/spin-up-subnet-a-validator-$1.log" 2>&1
  tail -n 3 "$HOME/.ipc/logs/spin-up-subnet-a-validator-$1.log"
  docker ps \
    --filter "name=^validator-$1-" \
    --format '{{.Names}}: {{.Status}}'
}
```

Helper: add a validator to the peer lists:

```bash
add_peer() {
  local log="$HOME/.ipc/logs/spin-up-subnet-a-validator-$1.log"
  local cmt res
  cmt=$(grep "CometBFT node ID:" -A 1 "$log" | tail -n 1 | tr -d '[:space:]')
  res=$(grep -o '/ip4/0.0.0.0/tcp/[0-9]*/p2p/[a-zA-Z0-9]*' "$log" | cut -d/ -f7)
  echo "validator $1: CometBFT ID $cmt, resolver ID $res"
  COMETBFT_PEERS="${COMETBFT_PEERS:+$COMETBFT_PEERS,}$cmt@validator-$1-cometbft:26656"
  RESOLVER_PEERS="${RESOLVER_PEERS:+$RESOLVER_PEERS,}/dns/validator-$1-fendermint/tcp/$2/p2p/$res"
}
```

### 2. Start validators 1 to 4, one at a time

Start validator 1:

```bash
start_validator 1 26656 26657 8545 26655 3030
```

Add validator 1 to the peer lists:

```bash
add_peer 1 26655
```

Start validator 2:

```bash
start_validator 2 26756 26757 8645 26755 3031
```

Add validator 2 to the peer lists:

```bash
add_peer 2 26755
```

Start validator 3:

```bash
start_validator 3 26856 26857 8745 26855 3032
```

Add validator 3 to the peer lists:

```bash
add_peer 3 26855
```

Start validator 4:

```bash
start_validator 4 26956 26957 8845 26955 3033
```

Add validator 4 to the peer lists:

```bash
add_peer 4 26955
```

### 3. Write the full peer lists

Set each validator's CometBFT peers:

```bash
for n in 1 2 3 4; do
  cargo-make make --makefile infra/fendermint/Makefile.toml \
    --env NODE_NAME="validator-$n" \
    --env SUBNET_ID="$SUBNET_ID" \
    --env HOME="$HOST_HOME_DIR" \
    --env PERSISTENT_PEERS="$(echo "$COMETBFT_PEERS" | tr ',' '\n' | grep -v "@validator-$n-cometbft:" | paste -sd, -)" \
    set-persistent-peers > /dev/null 2>&1 || echo "failed: validator $n"
done
```

### 4. Start the relayers

Start one relayer per validator:

```bash
for n in 1 2 3 4; do
  RUST_LOG=debug nohup ipc-cli --config-path "$HOME/.ipc/validator$n/config.toml" checkpoint relayer \
    --subnet "$SUBNET_ID" > "$HOME/.ipc/logs/relayer-subnet-a-validator$n.log" 2>&1 &
done
```
