## Changes

```sh
bitcoin-cli createwallet "user1"
bitcoin-cli generatetoaddress 102 "$(bitcoin-cli --rpcwallet=user1 getnewaddress)"
```

```md
ipc-cli checkpoint relayer --subnet

bash ./internal/miner.sh

bash ./internal/l2_block_checker.sh
```
