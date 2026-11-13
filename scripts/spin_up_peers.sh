# Peer lists of the validators of one subnet, sourced by the spin_up_subnet_*.sh scripts. The caller
# sets NODE_NAME_SUFFIX (validator n runs as node validator-<n><suffix>), CARGO_MAKE (the command
# that runs a task of the fendermint Makefile.toml) and SUBNET_ID.

# A validator redials its CometBFT persistent peers when a connection drops.
COMETBFT_PEERS=""
RESOLVER_PEERS=""

node_name() { echo "validator-$1${NODE_NAME_SUFFIX}"; }

# The ids derive from the node keys in the node's configuration; add_peer rejects an empty one.
cometbft_node_id() { docker exec "$(node_name "$1")-cometbft" cometbft show-node-id 2>/dev/null || true; }
resolver_node_id() {
    local node
    node=$(node_name "$1")
    docker exec "${node}-fendermint" fendermint key show-peer-id --public-key "/data/${node}/keys/network_key.pk" 2>/dev/null || true
}

add_peer() {
    local validator_num=$1
    local resolver_port=$2
    local cometbft_id=$3
    local resolver_id=$4
    local node
    node=$(node_name "$validator_num")

    if [ -z "$cometbft_id" ] || [ -z "$resolver_id" ]; then
        echo "Error: no node ids found for validator $validator_num." >&2
        exit 1
    fi
    COMETBFT_PEERS="${COMETBFT_PEERS:+$COMETBFT_PEERS,}${cometbft_id}@${node}-cometbft:26656"
    RESOLVER_PEERS="${RESOLVER_PEERS:+$RESOLVER_PEERS,}/dns/${node}-fendermint/tcp/${resolver_port}/p2p/${resolver_id}"
}

add_node_peer() { add_peer "$1" "$2" "$(cometbft_node_id "$1")" "$(resolver_node_id "$1")"; }

# CometBFT reloads its configuration at restart.
write_full_peer_lists() {
    local n node
    for n in "$@"; do
        node=$(node_name "$n")
        "${CARGO_MAKE[@]}" \
            --env NODE_NAME="$node" \
            --env SUBNET_ID="$SUBNET_ID" \
            --env PERSISTENT_PEERS="$(echo "$COMETBFT_PEERS" | tr ',' '\n' | grep -v "@${node}-cometbft:" | paste -sd, -)" \
            set-persistent-peers > /dev/null 2>&1 \
            || { echo "Error: failed to write the peer list of validator $n." >&2; exit 1; }
    done
}
