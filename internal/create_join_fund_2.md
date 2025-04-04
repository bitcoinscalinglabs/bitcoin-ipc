```sh
ipc-cli --config-path=$HOME/.ipc/validator1/config.toml subnet create --parent /b4 --min-validators 4 --bottomup-check-period 120 btc --min-validator-stake 10000000 --min-cross-msg-fee 10 --validator-whitelist 5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268,851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4,b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0,b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529

# mine a block

ipc-cli --config-path=$HOME/.ipc/validator1/config.toml subnet join --from 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --collateral=200000000 --ip 66.222.44.55:8080 --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n

ipc-cli --config-path=$HOME/.ipc/validator2/config.toml subnet join --from 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --collateral=110000000 --ip 66.222.44.55:8081 --backup-address bcrt1qs0ln9df4g59stzuh36892hvhg2y8z69999vwkx

ipc-cli --config-path=$HOME/.ipc/validator3/config.toml subnet join --from 0x646Aed5404567ae15648E9b9B0004cbAfb126949 --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --collateral=150000000 --ip 66.222.44.55:8082 --backup-address bcrt1qar9ak4wchftaf3z27tskzgc7lyx0hxemauexj0

ipc-cli --config-path=$HOME/.ipc/validator4/config.toml subnet join --from 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --collateral=180000000 --ip 66.222.44.55:8083 --backup-address bcrt1qxhnw85rz4euh532jjf9gd6wwkc3rhpj8dt23xk

# mine a block

ipc-cli --config-path=$HOME/.ipc/validator1/config.toml cross-msg fund --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --to 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 200000000

ipc-cli --config-path=$HOME/.ipc/validator2/config.toml cross-msg fund --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --to 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 200000000

ipc-cli --config-path=$HOME/.ipc/validator3/config.toml cross-msg fund --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --to 0x646Aed5404567ae15648E9b9B0004cbAfb126949 200000000

ipc-cli --config-path=$HOME/.ipc/validator4/config.toml cross-msg fund --subnet=/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --to 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf 200000000

# mine a block

bitcoin-cli generatetoaddress 1 "$(bitcoin-cli --rpcwallet=default getnewaddress)"
```

add to my config and to the config of validator 1

```toml
[[subnets]]
id = "/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy"

[subnets.config]
network_type = "fevm"
provider_http = "http://localhost:8545/"
gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
```


```sh
ipc-cli wallet balances --subnet=/b4/t410f326qqey6qqrgpe3klq5l7msm7fpfzyewhddufky --wallet-type btc

# RELAYER

ipc-cli --config-path=$HOME/.ipc/validator2/config.toml checkpoint relayer --subnet t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy

ipc-cli cross-msg fund --subnet=t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy btc --to 0xb628237ff4875b039ec1c3dedcf5fad93430ee4a 100000000

# Balance on the L1
bitcoin-cli -rpcwallet=user1 getbalance

# Balance on the L2
ipc-cli wallet balances --subnet /b4/t410f326qqey6qqrgpe3klq5l7msm7fpfzyewhddufky --wallet-type btc

ipc-cli cross-msg release --subnet /b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy --from 0x117607d776e450856c08fbfda27b92573779aeae btc --to bcrt1qjg2wwqd5qlwg64k2pgz7a5u3zy4kfpdd9n7rv3 111100

# SET DESTINATION SUBNET

ipc-cli cross-msg transfer --source-subnet t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy --destination-subnet t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy --source-address 0xb628237ff4875b039ec1c3dedcf5fad93430ee4a --destination-address 0x117607d776e450856c08fbfda27b92573779aeae 222200

ipc-cli cross-msg list-topdown-msgs --subnet=t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy --from 159 --to 165

```
