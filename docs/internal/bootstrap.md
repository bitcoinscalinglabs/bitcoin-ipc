# Demo Milestone 1: Creating a new subnet and running validator nodes

Validators:

```
SK   = 21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad
XPK  = 5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268
ADDR = 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8

SK   = 67308c2f3915f4c36135f267ed709418c2880025d669e4ada7a206842d53c146
XPK  = 851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4
ADDR = 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414

SK   = 994220215e4601d21a245f8f5e0c407f2f5733ce7907e128c3190c64f4ef443c
XPK  = b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0
ADDR = 0x646Aed5404567ae15648E9b9B0004cbAfb126949

SK   = ab3a1fafa925836386be55b12fdc92f208ebdad5ef96c0109e4bd06638dcb897
XPK  = b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529
ADDR = 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf
```


init config

```sh
# show config
cat ~/.ipc/config.toml
```

Import the keys:

```sh
ipc-cli wallet import --wallet-type btc --private-key "0x21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad"
ipc-cli wallet import --wallet-type btc --private-key "0x67308c2f3915f4c36135f267ed709418c2880025d669e4ada7a206842d53c146"
ipc-cli wallet import --wallet-type btc --private-key "0x994220215e4601d21a245f8f5e0c407f2f5733ce7907e128c3190c64f4ef443c"
ipc-cli wallet import --wallet-type btc --private-key "0xab3a1fafa925836386be55b12fdc92f208ebdad5ef96c0109e4bd06638dcb897"
```

```sh
ipc-cli wallet set-default --wallet-type btc --address 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8
```


```sh
ipc-cli --config-path=~/.ipc/validator1/config.toml subnet create --parent /b4 --min-validators 4 --bottomup-check-period 300 btc --min-validator-stake 100000000 --min-cross-msg-fee 10 --validator-whitelist 5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268,851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4,b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0,b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529

# mine a block

# replace address in the commands bellow

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=200000000 --ip 66.222.44.55:8080 --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=110000000 --ip 66.222.44.55:8081 --backup-address bcrt1qs0ln9df4g59stzuh36892hvhg2y8z69999vwkx

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0x646Aed5404567ae15648E9b9B0004cbAfb126949 --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=150000000 --ip 66.222.44.55:8082 --backup-address bcrt1qar9ak4wchftaf3z27tskzgc7lyx0hxemauexj0

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=180000000 --ip 66.222.44.55:8083 --backup-address bcrt1qxhnw85rz4euh532jjf9gd6wwkc3rhpj8dt23xk

# mine a block
```

```sh
# optional
fendermint genesis --genesis-file $HOME/.ipc/b4_t420fvqalkrdgdlqto4wn2zttntaisk22j2dupmsfnhhvpuizao6prx4xvnhuhm/genesis.json ipc from-parent --subnet-id "/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e" -p "http://127.0.0.1:3030/api" --parent-auth-token "asda123123jhaskjdhgbjsjhdj"
```

```sh
ipc-cli wallet export --wallet-type btc --address 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 --hex > ~/.ipc/validator_1.sk
ipc-cli wallet export --wallet-type btc --address 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 --hex > ~/.ipc/validator_2.sk
ipc-cli wallet export --wallet-type btc --address 0x646Aed5404567ae15648E9b9B0004cbAfb126949 --hex > ~/.ipc/validator_3.sk
ipc-cli wallet export --wallet-type btc --address 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf --hex > ~/.ipc/validator_4.sk
```

```sh
# Run first validator
cargo make --makefile infra/fendermint/Makefile.toml \
    -e NODE_NAME=validator-1 \
    -e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
    -e PRIVATE_KEY_PATH=$HOME/.ipc/validator1/validator.sk \
    -e CMT_P2P_HOST_PORT=26656 \
    -e CMT_RPC_HOST_PORT=26657 \
    -e ETHAPI_HOST_PORT=8545 \
    -e RESOLVER_HOST_PORT=26655 \
    -e PARENT_ENDPOINT="http://host.docker.internal:3030/api" \
    -e PARENT_AUTH_TOKEN="validator1_auth_token" \
    -e FM_PULL_SKIP=1 \
    child-validator

# Get cometbft node id
# And IPLD resolver

# Run second validator
cargo make --makefile infra/fendermint/Makefile.toml \
    -e NODE_NAME=validator-2 \
    -e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
    -e PRIVATE_KEY_PATH=$HOME/.ipc/validator2/validator.sk \
    -e CMT_P2P_HOST_PORT=26756 \
    -e CMT_RPC_HOST_PORT=26757 \
    -e ETHAPI_HOST_PORT=8645 \
    -e RESOLVER_HOST_PORT=26755 \
    -e BOOTSTRAPS=b082595e23d0814b202984313759a5e5bc6c6fbd@validator-1-cometbft:26656 \
    -e RESOLVER_BOOTSTRAPS=/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmDa2iAkkotWE2X65RfAGYFjU2QwyASesHcpM6nEacj336 \
    -e PARENT_ENDPOINT="http://host.docker.internal:3030/api" \
    -e PARENT_AUTH_TOKEN="asda123123jhaskjdhgbjsjhdj" \
    -e FM_PULL_SKIP=1 \
    child-validator

# Run third validator
cargo make --makefile infra/fendermint/Makefile.toml \
    -e NODE_NAME=validator-3 \
    -e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
    -e PRIVATE_KEY_PATH=$HOME/.ipc/validator3/validator.sk \
    -e CMT_P2P_HOST_PORT=26856 \
    -e CMT_RPC_HOST_PORT=26857 \
    -e ETHAPI_HOST_PORT=8745 \
    -e RESOLVER_HOST_PORT=26855 \
    -e BOOTSTRAPS=b082595e23d0814b202984313759a5e5bc6c6fbd@validator-1-cometbft:26656 \
    -e RESOLVER_BOOTSTRAPS=/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmDa2iAkkotWE2X65RfAGYFjU2QwyASesHcpM6nEacj336 \
    -e PARENT_ENDPOINT="http://host.docker.internal:3030/api" \
    -e PARENT_AUTH_TOKEN="asda123123jhaskjdhgbjsjhdj" \
    -e FM_PULL_SKIP=1 \
    child-validator

# Run fourth validator
cargo make --makefile infra/fendermint/Makefile.toml \
    -e NODE_NAME=validator-4 \
    -e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
    -e PRIVATE_KEY_PATH=$HOME/.ipc/validator4/validator.sk \
    -e CMT_P2P_HOST_PORT=26956 \
    -e CMT_RPC_HOST_PORT=26957 \
    -e ETHAPI_HOST_PORT=8845 \
    -e RESOLVER_HOST_PORT=26955 \
    -e BOOTSTRAPS=b082595e23d0814b202984313759a5e5bc6c6fbd@validator-1-cometbft:26656 \
    -e RESOLVER_BOOTSTRAPS=/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmDa2iAkkotWE2X65RfAGYFjU2QwyASesHcpM6nEacj336 \
    -e PARENT_ENDPOINT="http://host.docker.internal:3030/api" \
    -e PARENT_AUTH_TOKEN="asda123123jhaskjdhgbjsjhdj" \
    -e FM_PULL_SKIP=1 \
    child-validator
```

<!-- ```sh
cast chain-id --rpc-url http://127.0.0.1:8545
``` -->

```toml
# add config to ~/.ipc/config.toml
# explain

[[subnets]]
id = "/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e"

[subnets.config]
network_type = "fevm"
provider_http = "http://localhost:8545/"
gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
```

```sh
ipc-cli subnet rpc --network /b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e
```

## Cleanup

```sh
docker stop $(docker ps -aq)
docker rm $(docker ps -aq)

ipc-cli wallet list --wallet-type btc
ipc-cli wallet remove --wallet-type btc --address 0x646aed5404567ae15648e9b9b0004cbafb126949

for addr in (ipc-cli wallet list --wallet-type btc | grep "Address:" | awk '{print $2}')
    if test $addr != "default-key"
        echo "Removing wallet: $addr"
        ipc-cli wallet remove --wallet-type btc --address $addr
    end
end
```
