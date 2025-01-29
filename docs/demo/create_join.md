```sh
ipc-cli subnet create --parent /bip122:0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206 --min-validators 3 --bottomup-check-period 300 btc --min-validator-stake 10000000 --min-cross-msg-fee 10 --validator-whitelist 18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166,6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df,1dc9a71014974bcf298f71fbcfffa42e891d3f5376baa712f7379909a05b6be7,d789c59be13d8f0fe3e2a22ed062c821399c9486c3789d1fa2ca1b43c8246195

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "createsubnet",
    "params": {
	    "min_validator_stake": 100000000,
	    "min_validators": 3,
	    "bottomup_check_period": 5,
	    "active_validators_limit": 4,
	    "min_cross_msg_fee": 200,
	    "whitelist": [
		    "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
		    "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
		    "1dc9a71014974bcf298f71fbcfffa42e891d3f5376baa712f7379909a05b6be7",
				"d789c59be13d8f0fe3e2a22ed062c821399c9486c3789d1fa2ca1b43c8246195"
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
			"subnet_id": "/b4/f420fmn6fjcnhimmv47z7gzbhkegwckny6jlcnqcxppzsizxueab3huo6lczeni",
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
			"subnet_id": "/b4/f420fmn6fjcnhimmv47z7gzbhkegwckny6jlcnqcxppzsizxueab3huo6lczeni",
			"collateral": 110000000,
			"ip": "66.222.44.55:8081",
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
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/f420fmn6fjcnhimmv47z7gzbhkegwckny6jlcnqcxppzsizxueab3huo6lczeni",
			"collateral": 150000000,
			"ip": "66.222.44.55:8082",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "d789c59be13d8f0fe3e2a22ed062c821399c9486c3789d1fa2ca1b43c8246195"
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
		"subnet_id": "/b4/f420fmn6fjcnhimmv47z7gzbhkegwckny6jlcnqcxppzsizxueab3huo6lczeni"
	},
	"id": 1
}' | jq
```
