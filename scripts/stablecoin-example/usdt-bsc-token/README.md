## Test deployment of USDT on a Bitcoin-IPC subnet

The source code was fetched with no changes from https://bscscan.com/address/0x55d398326f99059fF775485246999027B3197955#code .

Relevant scripts:
- build.sh
- transact.sh

Before running them, please set `SUBNET_RPC_URL`. For a subnet running locally, this should be:
```
export SUBNET_RPC_URL=http://localhost:8545
```

Before running `transact.sh`, please set `TOKEN_ADDRESS` with the deployed address of the contract. For example:
```
export TOKEN_ADDRESS=0xb5942C2bdfE7ABFab243C51Cd44E6cB25E560C65
```