```sh
cargo make --makefile "../ipc/infra/fendermint/Makefile.toml" \
    -e NODE_NAME="validator-5" \
    -e SUBNET_ID="/b4/t410flmghovvd2antdosew6ozlngujsrycvls6eikjhy" \
    -e PRIVATE_KEY_PATH="$HOME/.ipc/validator5/validator.sk" \
    -e CMT_P2P_HOST_PORT="27056" \
    -e CMT_RPC_HOST_PORT="27057" \
    -e ETHAPI_HOST_PORT="8945" \
    -e RESOLVER_HOST_PORT="27055" \
    -e BOOTSTRAPS="35637abecdea5d43336b14adceeca3d69a6f9c43@validator-1-cometbft:26656" \
    -e RESOLVER_BOOTSTRAPS="/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmMJJDvQM2h7MhJSYjLkrdvKaMpYBqwxyqjNVTCsp3zYmq" \
    -e PARENT_ENDPOINT="http://host.docker.internal:3040/api" \
    -e PARENT_AUTH_TOKEN="asda123123jhaskjdhgbjsjhdj" \
    -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
    -e TOPDOWN_PROPOSAL_DELAY=0 \
    -e FM_PULL_SKIP=1 \
    child-validator

cargo make --makefile "../ipc/infra/fendermint/Makefile.toml" \
    -e NODE_NAME="validator-6" \
    -e SUBNET_ID="/b4/t410flmghovvd2antdosew6ozlngujsrycvls6eikjhy" \
    -e PRIVATE_KEY_PATH="$HOME/.ipc/validator5/validator.sk" \
    -e CMT_P2P_HOST_PORT="27156" \
    -e CMT_RPC_HOST_PORT="27157" \
    -e ETHAPI_HOST_PORT="9045" \
    -e RESOLVER_HOST_PORT="27155" \
    -e BOOTSTRAPS="5a6809d51d08eba75349d0e863cd6e636a97cac1@validator-1-cometbft:26656" \
    -e RESOLVER_BOOTSTRAPS="/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAm6RHA3FagSTkxjkmkiCe9Ew4Fzpn3mQGiMuYinFZeucCz" \
    -e PARENT_ENDPOINT="http://host.docker.internal:3040/api" \
    -e PARENT_AUTH_TOKEN="asda123123jhaskjdhgbjsjhdj" \
    -e TOPDOWN_CHAIN_HEAD_DELAY=0 \
    -e TOPDOWN_PROPOSAL_DELAY=0 \
    -e FM_PULL_SKIP=1 \
    child-validator


curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
		"subnet_id": "/b4/t410flmghovvd2antdosew6ozlngujsrycvls6eikjhy",
		"collateral": 20000000,
		"ip": "66.222.44.55:8080",
		"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
		"pubkey": "e327a66b169732bde49d827d90781327af558fc12d5cd2d5004e7551ec00c662"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
		"subnet_id": "/b4/t410flmghovvd2antdosew6ozlngujsrycvls6eikjhy",
		"collateral": 24000000,
		"ip": "66.222.44.55:8080",
		"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
		"pubkey": "71ac1eb874233999e11cd050f388f1dd6da9b446180fdf9b06740419cc487b6f"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getsubnet",
    "params": {
			"subnet_id": "/b4/t410flmghovvd2antdosew6ozlngujsrycvls6eikjhy"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getstakechanges",
    "params": {
			"subnet_id": "/b4/t410flmghovvd2antdosew6ozlngujsrycvls6eikjhy",
			"block_height": 1305
    },
    "id": 1
}' | jq
```
