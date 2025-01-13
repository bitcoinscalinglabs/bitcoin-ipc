```sh
ipc-cli subnet create --parent /r314159 --min-validator-stake 10000000 --min-validators 2 --bottomup-check-period 300 --permission-mode collateral --supply-source-kind native --min-cross-msg-fee 10 --validator-whitelist 18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166,6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df,1dc9a71014974bcf298f71fbcfffa42e891d3f5376baa712f7379909a05b6be7

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "createsubnet",
    "params": {
    "min_validator_stake": 100000000,
    "min_validators": 2,
    "bottomup_check_period": 5,
    "active_validators_limit": 2,
    "min_cross_msg_fee": 200,
    "whitelist": [
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
        "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
        "1dc9a71014974bcf298f71fbcfffa42e891d3f5376baa712f7379909a05b6be7"
    ]
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
    	"subnet_id": "BTC/4467317d030d3bcac27b897d05e7c1ad2aa138d669d017e512131852ccfbf287",
    	"collateral": 20000000,
		"ip": "66.222.44.55:8080",
		"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
		"pubkey": "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
    	"subnet_id": "BTC/4467317d030d3bcac27b897d05e7c1ad2aa138d669d017e512131852ccfbf287",
    	"collateral": 110000000,
		"ip": "66.222.44.55:8080",
		"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
		"pubkey": "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
"jsonrpc": "2.0",
"method": "getgenesisinfo",
"params": {
	"subnet_id": "BTC/4467317d030d3bcac27b897d05e7c1ad2aa138d669d017e512131852ccfbf287"
},
"id": 1
}' | jq
```
