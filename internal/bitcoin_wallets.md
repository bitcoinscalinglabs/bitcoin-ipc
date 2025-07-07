
```sh
bitcoin-cli -rpcwallet=validator1_subnets_watchonly listlabels

[
	"/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy-0",
  "/b4/t410fbr36gwprxna7jegp27w26plfkojdg4tha337ccy-1",
  "/b4/t410fcvxguh46ztf2ai3a2uull7nxazrrgbjlizguqxi-0",
  "/b4/t410fcvxguh46ztf2ai3a2uull7nxazrrgbjlizguqxi-1",
  "/b4/t410fe53urhgvv6rfdevyyhz6s5p75lnq3w33za2zzdy-0"
]
```

```sh
bitcoin-cli -rpcwallet=validator1_subnets_watchonly getaddressesbylabel /b4/t410fxyo353sn5oguanypndcnnvjbdy3uuqpidtunv7q-0

{
  "bcrt1pw9rm2vz742azym7snjerxzj7rqdwehjarrvka7mg82zhsu7t26uq2wlf2t": {
    "purpose": "receive"
  }
}
```

```sh
bitcoin-cli -rpcwallet=validator1_subnets_watchonly listunspent 0 9999999 '["bcrt1pcmdctveccxe09nl3lhaenyjddjggss95fkhf77vahnkqkn4eu95s3len9c"]'

[
  {
    "txid": "9a559484f70678d0ec2cd612a5ff3b5b3c0708d3cdd8f6bbfbe1de464c04b304",
    "vout": 1,
    "address": "bcrt1pw9rm2vz742azym7snjerxzj7rqdwehjarrvka7mg82zhsu7t26uq2wlf2t",
    "label": "/b4/t420fxm3vljgrnt4az4nbhwo74ih3b4lce2ecfzfrytqtzfnulhjfuagct52yci-1",
    "scriptPubKey": "51207147b5305eaaba226fd09cb2330a5e181aecde5d18d96efb683a857873cb56b8",
    "amount": 0.40000000,
    "confirmations": 1,
    "spendable": true,
    "solvable": true,
    "desc": "rawtr(7147b5305eaaba226fd09cb2330a5e181aecde5d18d96efb683a857873cb56b8)#waqaavyx",
    "parent_descs": [
      "addr(bcrt1pw9rm2vz742azym7snjerxzj7rqdwehjarrvka7mg82zhsu7t26uq2wlf2t)#0yjuh7at"
    ],
    "safe": true
  },
  {
    "txid": "f5f59e3551b7cb7b1f82b8c29a7024c3eb85fa207e9ff0b3e0f5c816f13f55a0",
    "vout": 1,
    "address": "bcrt1pw9rm2vz742azym7snjerxzj7rqdwehjarrvka7mg82zhsu7t26uq2wlf2t",
    "label": "/b4/t420fxm3vljgrnt4az4nbhwo74ih3b4lce2ecfzfrytqtzfnulhjfuagct52yci-1",
    "scriptPubKey": "51207147b5305eaaba226fd09cb2330a5e181aecde5d18d96efb683a857873cb56b8",
    "amount": 0.40000000,
    "confirmations": 2,
    "spendable": true,
    "solvable": true,
    "desc": "rawtr(7147b5305eaaba226fd09cb2330a5e181aecde5d18d96efb683a857873cb56b8)#waqaavyx",
    "parent_descs": [
      "addr(bcrt1pw9rm2vz742azym7snjerxzj7rqdwehjarrvka7mg82zhsu7t26uq2wlf2t)#0yjuh7at"
    ],
    "safe": true
  }
]
```
