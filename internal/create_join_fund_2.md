```sh
ipc-cli subnet create --parent /b4 --min-validators 4 --bottomup-check-period 100 btc --min-validator-stake 10000000 --min-cross-msg-fee 10 --validator-whitelist 5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268,851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4,b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0,b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529

# mine a block

ipc-cli subnet join --from 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --collateral=20000000 --ip 66.222.44.55:8080 --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n

ipc-cli subnet join --from 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --collateral=110000000 --ip 66.222.44.55:8081 --backup-address bcrt1qs0ln9df4g59stzuh36892hvhg2y8z69999vwkx

ipc-cli subnet join --from 0x646Aed5404567ae15648E9b9B0004cbAfb126949 --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --collateral=150000000 --ip 66.222.44.55:8082 --backup-address bcrt1qar9ak4wchftaf3z27tskzgc7lyx0hxemauexj0

ipc-cli subnet join --from 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --collateral=180000000 --ip 66.222.44.55:8083 --backup-address bcrt1qxhnw85rz4euh532jjf9gd6wwkc3rhpj8dt23xk

# mine a block

ipc-cli cross-msg fund --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --to 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 10000000

ipc-cli cross-msg fund --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --to 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 10000000

ipc-cli cross-msg fund --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --to 0x646Aed5404567ae15648E9b9B0004cbAfb126949 10000000

ipc-cli cross-msg fund --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --to 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf 10000000

bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=default getnewaddress)"
```

```sh
ipc-cli cross-msg fund --subnet=/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi btc --to 0xd5d3f056509373ecdf781be1a937090af1d9545c 10000000
```
