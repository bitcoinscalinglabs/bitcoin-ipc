```sh
export SubnetID="/b4/t410f3nguw4aj2i4osdjdf57rysam22cg5zs7y6zd3jq"

# Join validator 5
# --config-path ~/.ipc/validator5/config.toml
ipc-cli subnet join --from 0xb628237ff4875b039ec1c3dedcf5fad93430ee4a --subnet=$SubnetID btc --collateral=20000000 --ip 66.222.44.55:8080 --backup-address "$(bitcoin-cli --rpcwallet=validator5 getnewaddress)"

# Stake validator 3
# --config-path ~/.ipc/validator3/config.toml
ipc-cli subnet stake --subnet=$SubnetID btc --collateral 8500000 --validator-address 0x646Aed5404567ae15648E9b9B0004cbAfb126949

# Stake validator 1
# --config-path ~/.ipc/validator3/config.toml
ipc-cli subnet stake --subnet=$SubnetID btc --collateral 7700000 --validator-address 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8

# Unstake validator 1
# --config-path ~/.ipc/validator5/config.toml
ipc-cli subnet unstake --subnet=$SubnetID btc --collateral 1500000

# Join validator 6

cargo make --makefile "../ipc/infra/fendermint/Makefile.toml" \
        -e NODE_NAME="validator-6" \
        -e SUBNET_ID="$SubnetID" \
        -e PRIVATE_KEY_PATH="$HOME/.ipc/validator6/validator.sk" \
        -e CMT_P2P_HOST_PORT="27156" \
        -e CMT_RPC_HOST_PORT="27157" \
        -e ETHAPI_HOST_PORT="9045" \
        -e RESOLVER_HOST_PORT="27155" \
        -e BOOTSTRAPS="9bc45f24406931b2a79435301f974d97ecffd599@validator-1-cometbft:26656" \
        -e RESOLVER_BOOTSTRAPS="/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmD3QsxpMZRky7sGwfgC6ULrFowsT7nyvAmZxxK5Ty5kVK" \
        -e PARENT_ENDPOINT="http://host.docker.internal:3040/api" \
        -e PARENT_AUTH_TOKEN="asda123123jhaskjdhgbjsjhdj" \
        -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
        -e TOPDOWN_PROPOSAL_DELAY=0 \
        -e FM_PULL_SKIP=1 \
        child-validator

# --config-path ~/.ipc/validator6/config.toml
ipc-cli subnet join --from 0x117607d776e450856c08fbfda27b92573779aeae --subnet=$SubnetID btc --collateral=23500000 --ip 66.222.44.55:8080 --backup-address "$(bitcoin-cli --rpcwallet=validator6 getnewaddress)"


# Leave validator 1
# TODO


```
